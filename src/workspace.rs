use std::collections::{BTreeMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs4::fs_std::FileExt;
use ignore::WalkBuilder;

use crate::catalog::Catalog;
use crate::data::{DuckDbProfiler, execute_query};
use crate::extract::{
    classify, extension, extract_symbols, extract_text, is_default_excluded, is_sensitive,
    mtime_ns, should_extract_text,
};
use crate::model::{
    CatalogFileInput, ContentExcerpt, DatasetProfile, FileKind, IndexOptions, IndexReport,
    IndexStatus, InspectResult, NotifyReport, QueryRequest, QueryResult, SearchDocument, SearchHit,
    SymbolRecord,
};
use crate::search::SearchIndex;
use crate::snapshot::{SnapshotManifest, publish};

const DEFAULT_INSPECT_LINES: usize = 120;
const DEFAULT_INSPECT_CHARS: usize = 32 * 1024;

pub struct WorkspaceIndex {
    index_dir: PathBuf,
    catalog: Catalog,
    search: SearchIndex,
}

impl WorkspaceIndex {
    pub fn open(index_dir: impl AsRef<Path>) -> Result<Self> {
        let index_dir = index_dir.as_ref();
        fs::create_dir_all(index_dir)
            .with_context(|| format!("create AWI index directory {}", index_dir.display()))?;
        let catalog = Catalog::open(&index_dir.join("catalog.sqlite3"))?;
        let search = SearchIndex::open(&index_dir.join("tantivy"))?;
        Ok(Self {
            index_dir: index_dir.to_owned(),
            catalog,
            search,
        })
    }

    pub fn index_root(
        &mut self,
        root: impl AsRef<Path>,
        options: &IndexOptions,
    ) -> Result<IndexReport> {
        let root = fs::canonicalize(root.as_ref())
            .with_context(|| format!("resolve root {}", root.as_ref().display()))?;
        if !root.is_dir() {
            anyhow::bail!("index root is not a directory: {}", root.display());
        }

        let _writer_lock = acquire_writer_lock(&self.index_dir)?;
        let (root_id, generation) = self.catalog.start_generation(&root)?;
        let result = self.index_root_inner(&root, root_id, generation, options);
        if let Err(error) = &result {
            let _ = self
                .catalog
                .fail_generation(generation, &format!("{error:#}"));
        }
        result
    }

    pub fn notify_paths(
        &mut self,
        paths: &[PathBuf],
        options: &IndexOptions,
    ) -> Result<NotifyReport> {
        if paths.is_empty() {
            anyhow::bail!("notify requires at least one path");
        }
        let roots = self.catalog.roots()?;
        if roots.is_empty() {
            anyhow::bail!("notify requires an indexed root; run awi reconcile <root> first");
        }

        let mut grouped = BTreeMap::<i64, (PathBuf, Vec<PathBuf>)>::new();
        for path in paths {
            let path = notification_path(path)?;
            let (root_id, root) = roots
                .iter()
                .find(|(_, root)| path.starts_with(root))
                .with_context(|| {
                    format!(
                        "notified path {} is outside every indexed root",
                        path.display()
                    )
                })?;
            grouped
                .entry(*root_id)
                .or_insert_with(|| (root.clone(), Vec::new()))
                .1
                .push(path);
        }

        let _writer_lock = acquire_writer_lock(&self.index_dir)?;
        let mut output = NotifyReport {
            requested: paths.len() as u64,
            roots: Vec::with_capacity(grouped.len()),
        };
        for (root_id, (root, mut paths)) in grouped {
            paths.sort();
            paths.dedup();
            let (_, generation) = self.catalog.start_generation(&root)?;
            let result = self.notify_root_inner(&root, root_id, generation, &paths, options);
            if let Err(error) = &result {
                let _ = self
                    .catalog
                    .fail_generation(generation, &format!("{error:#}"));
            }
            output.roots.push(result?);
        }
        Ok(output)
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.search_filtered(query, limit, &[], &[], None)
    }

    pub fn search_filtered(
        &self,
        query: &str,
        limit: usize,
        roots: &[PathBuf],
        kinds: &[String],
        path_prefix: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        if query.trim().is_empty() {
            anyhow::bail!("search query must not be empty");
        }
        let registered_roots = self.catalog.roots()?;
        let mut root_filter = HashSet::new();
        for root in roots {
            let canonical = fs::canonicalize(root)
                .with_context(|| format!("resolve search root {}", root.display()))?;
            let mut matched = false;
            for (_, registered) in &registered_roots {
                if registered == &canonical || registered.starts_with(&canonical) {
                    root_filter.insert(registered.to_string_lossy().into_owned());
                    matched = true;
                }
            }
            if !matched {
                anyhow::bail!(
                    "search root {} does not contain an indexed workspace root",
                    canonical.display()
                );
            }
        }
        let kind_filter = kinds
            .iter()
            .map(|kind| FileKind::try_from(kind.as_str()))
            .collect::<Result<Vec<_>>>()?;
        let filtered = !root_filter.is_empty() || !kind_filter.is_empty() || path_prefix.is_some();
        let overfetch = if filtered {
            limit.saturating_mul(20).clamp(200, 1_000)
        } else {
            limit.saturating_mul(4).max(20)
        };
        let candidates = self.search.search(query, overfetch, overfetch)?;
        let mut hits = Vec::with_capacity(limit);
        for candidate in candidates {
            let Some(file) = self.catalog.current_file_by_id(candidate.file_id)? else {
                continue;
            };
            if file.generation != candidate.generation
                || (!root_filter.is_empty() && !root_filter.contains(&file.root))
                || (!kind_filter.is_empty() && !kind_filter.contains(&file.kind))
                || path_prefix.is_some_and(|prefix| {
                    !file.absolute_path.starts_with(prefix)
                        && !file.relative_path.starts_with(prefix)
                })
            {
                continue;
            }
            hits.push(SearchHit {
                file_id: file.id,
                path: file.absolute_path,
                relative_path: file.relative_path,
                kind: file.kind,
                experiment: file.experiment,
                size_bytes: file.size_bytes,
                mtime_ns: file.mtime_ns,
                generation: file.generation,
                score: candidate.score,
                matched_lanes: candidate.lanes,
                preview: candidate.preview,
            });
            if hits.len() == limit {
                break;
            }
        }
        Ok(hits)
    }

    pub fn inspect(&self, path: impl AsRef<Path>) -> Result<InspectResult> {
        self.inspect_excerpt(path, 1, DEFAULT_INSPECT_LINES, DEFAULT_INSPECT_CHARS)
    }

    pub fn inspect_excerpt(
        &self,
        path: impl AsRef<Path>,
        start_line: usize,
        max_lines: usize,
        max_chars: usize,
    ) -> Result<InspectResult> {
        if start_line == 0 || max_lines == 0 || max_chars == 0 {
            anyhow::bail!("inspect excerpt limits must be positive");
        }
        let path = fs::canonicalize(path.as_ref())
            .with_context(|| format!("resolve path {}", path.as_ref().display()))?;
        let absolute_path = path.to_string_lossy();
        let file = self
            .catalog
            .current_file_by_path(&absolute_path)?
            .with_context(|| {
                format!("path is not present in the current index: {absolute_path}")
            })?;
        let symbols = self.catalog.symbols_for(file.id)?;
        let dataset = self.catalog.dataset_profile_for(file.id)?;
        let content = content_excerpt(&path, &file, start_line, max_lines, max_chars)?;
        Ok(InspectResult {
            file,
            symbols,
            dataset,
            content,
        })
    }

    pub fn status(&self) -> Result<IndexStatus> {
        self.catalog.status()
    }

    pub fn query(&self, request: &QueryRequest) -> Result<QueryResult> {
        let registered_roots = self.catalog.roots()?;
        for root in &request.roots {
            let canonical = fs::canonicalize(root)
                .with_context(|| format!("resolve query root {}", root.display()))?;
            if !registered_roots
                .iter()
                .any(|(_, registered)| registered == &canonical)
            {
                anyhow::bail!(
                    "query root {} is not an indexed workspace root",
                    canonical.display()
                );
            }
        }
        execute_query(request)
    }

    pub fn publish_snapshot(&self, publish_dir: impl AsRef<Path>) -> Result<SnapshotManifest> {
        let _writer_lock = acquire_writer_lock(&self.index_dir)?;
        self.catalog.checkpoint()?;
        let generation = self
            .catalog
            .latest_completed_generation()?
            .context("cannot publish an index without a completed generation")?;
        publish(&self.index_dir, publish_dir.as_ref(), generation)
    }

    fn index_root_inner(
        &mut self,
        root: &Path,
        root_id: i64,
        generation: i64,
        options: &IndexOptions,
    ) -> Result<IndexReport> {
        let profiler =
            DuckDbProfiler::new(options.duckdb_timeout_seconds, options.max_profile_bytes);
        let mut report = IndexReport {
            root: root.to_owned(),
            generation,
            ..IndexReport::default()
        };
        let mut documents = Vec::new();

        let mut builder = WalkBuilder::new(root);
        builder
            .hidden(false)
            .git_ignore(true)
            .git_exclude(true)
            .parents(true)
            .follow_links(false)
            .add_custom_ignore_filename(".awiignore");
        let walker = builder
            .filter_entry(|entry| !is_default_excluded(entry.path()))
            .build();

        for entry in walker {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    report.failed += 1;
                    self.catalog.record_failure(
                        generation,
                        "<walk>",
                        "discovery",
                        &error.to_string(),
                    )?;
                    continue;
                }
            };
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_file() || is_sensitive(entry.path()) {
                continue;
            }

            report.discovered += 1;
            let path = entry.into_path();
            if let Err(error) = self.index_file(
                root,
                root_id,
                generation,
                &path,
                options,
                &profiler,
                &mut documents,
                &mut report,
            ) {
                report.failed += 1;
                let path_text = path.to_string_lossy();
                if let Ok(Some(existing)) = self.catalog.existing_file(&path_text) {
                    let _ = self.catalog.mark_seen_only(existing.id, generation);
                }
                self.catalog.record_failure(
                    generation,
                    &path_text,
                    "index_file",
                    &format!("{error:#}"),
                )?;
            }
        }

        let deleted_paths = self.catalog.mark_missing_deleted(root_id, generation)?;
        report.deleted = deleted_paths.len() as u64;
        self.search.apply_changes(&documents, &deleted_paths)?;
        self.catalog.complete_generation(root_id, generation)?;
        Ok(report)
    }

    fn notify_root_inner(
        &mut self,
        root: &Path,
        root_id: i64,
        generation: i64,
        paths: &[PathBuf],
        options: &IndexOptions,
    ) -> Result<IndexReport> {
        let profiler =
            DuckDbProfiler::new(options.duckdb_timeout_seconds, options.max_profile_bytes);
        let mut report = IndexReport {
            root: root.to_owned(),
            generation,
            ..IndexReport::default()
        };
        let mut documents = Vec::new();
        let mut deleted_paths = Vec::new();

        for path in paths {
            report.discovered += 1;
            if !path.exists() {
                let path_text = path.to_string_lossy();
                if self
                    .catalog
                    .mark_path_deleted(root_id, &path_text, generation)?
                {
                    deleted_paths.push(path_text.into_owned());
                    report.deleted += 1;
                } else {
                    report.unchanged += 1;
                }
                continue;
            }
            if !path.is_file() {
                report.failed += 1;
                self.catalog.record_failure(
                    generation,
                    &path.to_string_lossy(),
                    "notify",
                    "notified paths must be files; use reconcile for directories",
                )?;
                continue;
            }
            if is_sensitive(path) {
                report.failed += 1;
                self.catalog.record_failure(
                    generation,
                    &path.to_string_lossy(),
                    "notify",
                    "sensitive path is excluded",
                )?;
                continue;
            }
            if let Err(error) = self.index_file(
                root,
                root_id,
                generation,
                path,
                options,
                &profiler,
                &mut documents,
                &mut report,
            ) {
                report.failed += 1;
                self.catalog.record_failure(
                    generation,
                    &path.to_string_lossy(),
                    "notify",
                    &format!("{error:#}"),
                )?;
            }
        }

        self.search.apply_changes(&documents, &deleted_paths)?;
        self.catalog.complete_generation(root_id, generation)?;
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    fn index_file(
        &mut self,
        root: &Path,
        root_id: i64,
        generation: i64,
        path: &Path,
        options: &IndexOptions,
        profiler: &DuckDbProfiler,
        documents: &mut Vec<SearchDocument>,
        report: &mut IndexReport,
    ) -> Result<()> {
        let metadata =
            fs::metadata(path).with_context(|| format!("read metadata for {}", path.display()))?;
        let size_bytes = metadata.len();
        let mtime_ns = mtime_ns(&metadata);
        let kind = classify(path);
        let absolute_path = path.to_string_lossy().into_owned();
        let relative_path = path
            .strip_prefix(root)
            .with_context(|| format!("strip root from {}", path.display()))?
            .to_string_lossy()
            .into_owned();
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_owned();
        let experiment = experiment_name(Path::new(&relative_path));
        let existing = self.catalog.existing_file(&absolute_path)?;

        if existing.as_ref().is_some_and(|state| {
            state.size_bytes == size_bytes && state.mtime_ns == mtime_ns && state.kind == kind
        }) {
            let file_id = existing.as_ref().expect("checked above").id;
            self.catalog
                .mark_seen(file_id, generation, size_bytes, mtime_ns)?;
            report.unchanged += 1;
            return Ok(());
        }

        let text = if should_extract_text(kind, size_bytes, options.max_content_bytes) {
            extract_text(path, options.max_content_bytes)?
        } else {
            crate::extract::TextExtraction {
                content: None,
                content_hash: None,
                preview: String::new(),
                status: "metadata_only".to_owned(),
            }
        };

        if existing.as_ref().is_some_and(|state| {
            state.content_hash.is_some()
                && state.content_hash == text.content_hash
                && state.kind == kind
        }) {
            let file_id = existing.as_ref().expect("checked above").id;
            self.catalog
                .mark_seen(file_id, generation, size_bytes, mtime_ns)?;
            report.unchanged += 1;
            return Ok(());
        }

        let (symbols, code_status) = extract_code(path, kind, text.content.as_deref())?;
        let profile = profiler.profile(path, size_bytes);
        let extraction_status = combined_status(&text.status, &code_status, profile.as_ref());
        let input = CatalogFileInput {
            root_id,
            absolute_path: absolute_path.clone(),
            relative_path,
            name: name.clone(),
            extension: extension(path),
            kind,
            experiment: experiment.clone(),
            size_bytes,
            mtime_ns,
            content_hash: text.content_hash,
            generation,
            content_indexed: text.content.is_some(),
            extraction_status,
        };
        let file_id = self.catalog.upsert_file(&input)?;
        self.catalog.replace_symbols(file_id, &symbols)?;
        self.catalog
            .replace_dataset_profile(file_id, profile.as_ref())?;

        let symbols_text = symbols
            .iter()
            .flat_map(|symbol| {
                [
                    symbol.name.as_str(),
                    symbol.signature.as_deref().unwrap_or(""),
                ]
            })
            .collect::<Vec<_>>()
            .join(" ");
        let schema = profile
            .as_ref()
            .map(DatasetProfile::schema_text)
            .unwrap_or_default();
        documents.push(SearchDocument {
            file_id,
            path: absolute_path,
            name,
            experiment: experiment.unwrap_or_default(),
            kind: kind.as_str().to_owned(),
            content: text.content.unwrap_or_default(),
            symbols: symbols_text,
            schema,
            preview: text.preview,
            generation,
        });

        report.indexed += 1;
        if !input.content_indexed {
            report.metadata_only += 1;
        }
        Ok(())
    }
}

fn content_excerpt(
    path: &Path,
    file: &crate::FileRecord,
    start_line: usize,
    max_lines: usize,
    max_chars: usize,
) -> Result<Option<ContentExcerpt>> {
    if !file.content_indexed {
        return Ok(None);
    }
    let metadata =
        fs::metadata(path).with_context(|| format!("read metadata for {}", path.display()))?;
    if metadata.len() != file.size_bytes || mtime_ns(&metadata) != file.mtime_ns {
        anyhow::bail!(
            "indexed file changed since generation {}; run awi notify before inspection: {}",
            file.generation,
            path.display()
        );
    }
    let source = fs::read_to_string(path)
        .with_context(|| format!("read indexed text {}", path.display()))?;
    let mut text = String::new();
    let mut end_line = start_line.saturating_sub(1);
    let mut truncated = false;
    for (included_lines, (offset, line)) in
        source.lines().enumerate().skip(start_line - 1).enumerate()
    {
        if included_lines == max_lines {
            truncated = true;
            break;
        }
        let line_number = offset + 1;
        let rendered = format!("{line_number}: {line}\n");
        let used = text.chars().count();
        let remaining = max_chars.saturating_sub(used);
        if rendered.chars().count() > remaining {
            text.extend(rendered.chars().take(remaining));
            end_line = line_number;
            truncated = true;
            break;
        }
        text.push_str(&rendered);
        end_line = line_number;
    }
    Ok(Some(ContentExcerpt {
        start_line,
        end_line,
        text,
        truncated,
    }))
}

fn notification_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    if absolute.exists() {
        return fs::canonicalize(&absolute)
            .with_context(|| format!("resolve notified path {}", absolute.display()));
    }
    let parent = absolute
        .parent()
        .context("notified path has no parent directory")?;
    let name = absolute
        .file_name()
        .context("notified path has no file name")?;
    Ok(fs::canonicalize(parent)
        .with_context(|| format!("resolve notified path parent {}", parent.display()))?
        .join(name))
}

fn acquire_writer_lock(index_dir: &Path) -> Result<File> {
    let path = index_dir.join("writer.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("open AWI writer lock {}", path.display()))?;
    file.lock_exclusive()
        .with_context(|| format!("lock AWI writer {}", path.display()))?;
    Ok(file)
}

fn extract_code(
    path: &Path,
    kind: FileKind,
    content: Option<&str>,
) -> Result<(Vec<SymbolRecord>, String)> {
    if kind != FileKind::Source {
        return Ok((Vec::new(), "not_source".to_owned()));
    }
    let Some(content) = content else {
        return Ok((Vec::new(), "source_not_read".to_owned()));
    };
    extract_symbols(path, content)
}

fn experiment_name(relative_path: &Path) -> Option<String> {
    let first = relative_path.components().next()?.as_os_str().to_str()?;
    if relative_path.components().count() > 1 {
        Some(first.to_owned())
    } else {
        None
    }
}

fn combined_status(
    text_status: &str,
    code_status: &str,
    profile: Option<&DatasetProfile>,
) -> String {
    let mut parts = vec![text_status, code_status];
    if let Some(profile) = profile {
        parts.push(&profile.status);
    }
    parts.join(",")
}
