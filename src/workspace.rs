use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fs4::fs_std::FileExt;
use ignore::WalkBuilder;

use crate::catalog::Catalog;
use crate::data::{DuckDbProfiler, execute_query};
use crate::extract::{
    classify, contains_sensitive_agent_content, extension, extract_agent_document, extract_symbols,
    extract_text, index_preview, is_default_excluded, is_sensitive, mtime_ns,
    normalize_agent_memory_content, should_extract_text,
};
use crate::memory::{discover_agent_memory_sources, memory_metadata};
use crate::metrics::LatencyWindow;
use crate::model::{
    AgentDocumentRole, AgentMemoryIndexReport, AgentMemoryRefreshReport, AgentMemorySource,
    CatalogFileInput, ContentExcerpt, DatasetProfile, FileKind, IndexOptions, IndexReport,
    IndexStatus, InspectResult, NotifyReport, QueryRequest, QueryResult, RetrievalStatus,
    SearchDocument, SearchHit, SemanticBuildReport, SymbolRecord,
};
use crate::search::SearchIndex;
use crate::semantic::{SemanticIndex, merge_semantic_candidates};
use crate::snapshot::{SnapshotManifest, publish};

const DEFAULT_INSPECT_LINES: usize = 120;
const DEFAULT_INSPECT_CHARS: usize = 32 * 1024;
const APPLICABLE_INSTRUCTION_BOOST: f32 = 1.0;
const INSTRUCTION_DEPTH_BOOST: f32 = 0.001;
const EXACT_PROJECT_MEMORY_BOOST: f32 = 0.08;
const GLOBAL_MEMORY_BOOST: f32 = 0.01;
const MEMORY_LAYER_BOOST: f32 = 0.004;
const MAX_MEMORY_RECENCY_BOOST: f32 = 0.005;
const EXACT_MEMORY_FILENAME_BOOST: f32 = 0.25;
const EXACT_MEMORY_NAME_BOOST: f32 = 0.22;
const MEMORY_DESCRIPTION_BOOST: f32 = 0.04;
const AGGREGATE_MEMORY_PENALTY: f32 = 0.08;
const EXACT_SYMBOL_BOOST: f32 = 1.5;
const SAME_DIRECTORY_MMR_PENALTY: f32 = 0.00125;
const MATCH_PREVIEW_CHARS: usize = 1_000;
const SEARCH_CACHE_CAPACITY: usize = 256;

#[derive(Default)]
struct RetrievalMetricsState {
    searches: AtomicU64,
    in_flight: AtomicU64,
    cache_hits: AtomicU64,
    semantic_fallbacks: AtomicU64,
    total: LatencyWindow,
    lexical: LatencyWindow,
    semantic: LatencyWindow,
    fusion: LatencyWindow,
    catalog_filter: LatencyWindow,
    preview: LatencyWindow,
    cache: Mutex<SearchCache>,
}

#[derive(Default)]
struct SearchCache {
    sequence: u64,
    entries: HashMap<String, (u64, Vec<SearchHit>)>,
}

struct SearchInFlight<'a> {
    counter: &'a AtomicU64,
}

impl Drop for SearchInFlight<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

struct RetrievalDurations {
    total: Duration,
    lexical: Duration,
    semantic: Option<Duration>,
    fusion: Duration,
    catalog_filter: Duration,
    preview: Duration,
    semantic_fallback: bool,
}

impl RetrievalMetricsState {
    fn begin(&self) -> SearchInFlight<'_> {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        SearchInFlight {
            counter: &self.in_flight,
        }
    }

    fn record(&self, durations: RetrievalDurations) {
        self.searches.fetch_add(1, Ordering::Relaxed);
        if durations.semantic_fallback {
            self.semantic_fallbacks.fetch_add(1, Ordering::Relaxed);
        }
        self.total.record(durations.total);
        self.lexical.record(durations.lexical);
        if let Some(semantic) = durations.semantic {
            self.semantic.record(semantic);
        }
        self.fusion.record(durations.fusion);
        self.catalog_filter.record(durations.catalog_filter);
        self.preview.record(durations.preview);
    }

    fn get_cached(&self, key: &str) -> Option<Vec<SearchHit>> {
        let mut cache = self.cache.lock().expect("search cache mutex poisoned");
        let next_sequence = cache.sequence.saturating_add(1);
        let hits = cache.entries.get_mut(key)?;
        hits.0 = next_sequence;
        let output = hits.1.clone();
        cache.sequence = next_sequence;
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
        Some(output)
    }

    fn cache(&self, key: String, hits: Vec<SearchHit>) {
        let mut cache = self.cache.lock().expect("search cache mutex poisoned");
        cache.sequence = cache.sequence.saturating_add(1);
        let sequence = cache.sequence;
        cache.entries.insert(key, (sequence, hits));
        if cache.entries.len() > SEARCH_CACHE_CAPACITY
            && let Some(oldest) = cache
                .entries
                .iter()
                .min_by_key(|(_, (sequence, _))| sequence)
                .map(|(key, _)| key.clone())
        {
            cache.entries.remove(&oldest);
        }
    }

    fn snapshot(&self) -> RetrievalStatus {
        RetrievalStatus {
            searches: self.searches.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            semantic_fallbacks: self.semantic_fallbacks.load(Ordering::Relaxed),
            total: self.total.snapshot(),
            lexical: self.lexical.snapshot(),
            semantic: self.semantic.snapshot(),
            fusion: self.fusion.snapshot(),
            catalog_filter: self.catalog_filter.snapshot(),
            preview: self.preview.snapshot(),
        }
    }
}

pub struct WorkspaceIndex {
    index_dir: PathBuf,
    catalog: Catalog,
    search: SearchIndex,
    memory_search: SearchIndex,
    semantic: SemanticIndex,
    metrics: Arc<RetrievalMetricsState>,
}

impl WorkspaceIndex {
    pub fn open(index_dir: impl AsRef<Path>) -> Result<Self> {
        let index_dir = index_dir.as_ref();
        fs::create_dir_all(index_dir)
            .with_context(|| format!("create AWI index directory {}", index_dir.display()))?;
        let catalog = Catalog::open(&index_dir.join("catalog.sqlite3"))?;
        let search = SearchIndex::open(&index_dir.join("tantivy"))?;
        let memory_search = SearchIndex::open(&index_dir.join("memory-tantivy"))?;
        let semantic = SemanticIndex::open(index_dir)?;
        Ok(Self {
            index_dir: index_dir.to_owned(),
            catalog,
            search,
            memory_search,
            semantic,
            metrics: Arc::new(RetrievalMetricsState::default()),
        })
    }

    pub(crate) fn open_reader_at(&self, index_dir: &Path) -> Result<Self> {
        let catalog = Catalog::open(&index_dir.join("catalog.sqlite3"))?;
        let search = SearchIndex::open(&index_dir.join("tantivy"))?;
        let memory_search = SearchIndex::open(&index_dir.join("memory-tantivy"))?;
        Ok(Self {
            index_dir: index_dir.to_owned(),
            catalog,
            search,
            memory_search,
            semantic: self.semantic.retarget(index_dir),
            metrics: Arc::clone(&self.metrics),
        })
    }

    pub(crate) fn index_dir(&self) -> &Path {
        &self.index_dir
    }

    pub fn index_root(
        &mut self,
        root: impl AsRef<Path>,
        options: &IndexOptions,
    ) -> Result<IndexReport> {
        let requested_root = root.as_ref();
        let root = match fs::canonicalize(requested_root) {
            Ok(root) => root,
            Err(error) => {
                let absolute = if requested_root.is_absolute() {
                    requested_root.to_owned()
                } else {
                    std::env::current_dir()?.join(requested_root)
                };
                let registered_roots = self.catalog.roots()?;
                let root = if let Some((_, registered)) = registered_roots
                    .iter()
                    .find(|(_, registered)| registered == &absolute)
                {
                    registered.clone()
                } else {
                    notification_path(requested_root)?
                };
                let is_registered_memory =
                    self.catalog.agent_memory_source_for_path(&root)?.is_some();
                if !matches!(
                    classify(requested_root),
                    FileKind::AgentInstructions | FileKind::AgentSkill
                ) && !is_registered_memory
                {
                    return Err(error)
                        .with_context(|| format!("resolve root {}", requested_root.display()));
                }
                if !registered_roots
                    .iter()
                    .any(|(_, registered)| registered == &root)
                {
                    return Err(error)
                        .with_context(|| format!("resolve root {}", requested_root.display()));
                }
                let _writer_lock = acquire_writer_lock(&self.index_dir)?;
                let (root_id, generation) = self.catalog.start_generation(&root)?;
                let path = root.to_string_lossy().into_owned();
                let deleted_paths = if is_registered_memory {
                    self.catalog.mark_missing_deleted(root_id, generation)?
                } else {
                    self.catalog
                        .mark_path_deleted(root_id, &path, generation)?
                        .then_some(path)
                        .into_iter()
                        .collect::<Vec<_>>()
                };
                let deleted = deleted_paths.len() as u64;
                self.apply_search_changes(&[], &deleted_paths)?;
                self.catalog.complete_generation(root_id, generation)?;
                return Ok(IndexReport {
                    root,
                    generation,
                    discovered: 0,
                    indexed: 0,
                    unchanged: u64::from(deleted == 0),
                    deleted,
                    metadata_only: 0,
                    failed: 0,
                });
            }
        };
        let is_registered_memory = self.catalog.agent_memory_source_for_path(&root)?.is_some();
        if !(root.is_dir()
            || root.is_file()
                && matches!(
                    classify(&root),
                    FileKind::AgentInstructions | FileKind::AgentSkill
                )
            || root.is_file() && is_registered_memory)
        {
            anyhow::bail!(
                "index root must be a directory, AGENTS.md, SKILL.md, or registered Agent memory: {}",
                root.display()
            );
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

    pub fn index_agent_memories(
        &mut self,
        project_root: impl AsRef<Path>,
        include_raw: bool,
        options: &IndexOptions,
    ) -> Result<AgentMemoryIndexReport> {
        let project_root = fs::canonicalize(project_root.as_ref()).with_context(|| {
            format!(
                "resolve Agent memory project root {}",
                project_root.as_ref().display()
            )
        })?;
        {
            let _writer_lock = acquire_writer_lock(&self.index_dir)?;
            self.catalog
                .register_agent_memory_project(&project_root, include_raw)?;
        }
        let sources = discover_agent_memory_sources(&project_root, include_raw)?;
        let mut reports = Vec::with_capacity(sources.len());
        for source in &sources {
            reports.push(self.index_agent_memory_source(source, options)?);
        }
        Ok(AgentMemoryIndexReport {
            project_root,
            include_raw,
            sources,
            reports,
        })
    }

    pub fn refresh_agent_memories(
        &mut self,
        options: &IndexOptions,
    ) -> Result<AgentMemoryRefreshReport> {
        let projects = self.catalog.agent_memory_projects()?;
        let managed_projects = projects
            .iter()
            .map(|(project_root, _)| project_root.clone())
            .collect::<HashSet<_>>();
        let mut refresh = AgentMemoryRefreshReport {
            projects: projects.len() as u64,
            ..Default::default()
        };
        let mut discovered_paths = HashSet::new();
        for (project_root, include_raw) in projects {
            let report = self.index_agent_memories(project_root, include_raw, options)?;
            refresh.sources = refresh.sources.saturating_add(report.sources.len() as u64);
            discovered_paths.extend(report.sources.iter().map(|source| source.path.clone()));
            for source_report in report.reports {
                refresh.indexed = refresh.indexed.saturating_add(source_report.indexed);
                refresh.deleted = refresh.deleted.saturating_add(source_report.deleted);
            }
        }
        for source in self.catalog.agent_memory_sources()? {
            if discovered_paths.contains(&source.path) {
                continue;
            }
            let managed = source
                .workspace_root
                .as_ref()
                .map_or(!managed_projects.is_empty(), |workspace| {
                    managed_projects.contains(workspace)
                });
            if managed {
                refresh.deleted = refresh
                    .deleted
                    .saturating_add(self.retire_agent_memory_source(&source.path)?);
            }
        }
        Ok(refresh)
    }

    fn retire_agent_memory_source(&mut self, path: &Path) -> Result<u64> {
        let _writer_lock = acquire_writer_lock(&self.index_dir)?;
        let (root_id, generation) = self.catalog.start_generation(path)?;
        let result = (|| {
            let deleted_paths = self.catalog.mark_missing_deleted(root_id, generation)?;
            self.apply_search_changes(&[], &deleted_paths)?;
            self.catalog.complete_generation(root_id, generation)?;
            self.catalog.deactivate_agent_memory_source(path)?;
            Ok(deleted_paths.len() as u64)
        })();
        if let Err(error) = &result {
            let _ = self
                .catalog
                .fail_generation(generation, &format!("{error:#}"));
        }
        result
    }

    pub fn index_agent_memory_source(
        &mut self,
        source: &AgentMemorySource,
        options: &IndexOptions,
    ) -> Result<IndexReport> {
        let path = fs::canonicalize(&source.path)
            .with_context(|| format!("resolve Agent memory source {}", source.path.display()))?;
        let mut source = source.clone();
        source.path = path;
        if let Some(workspace_root) = &source.workspace_root {
            source.workspace_root = Some(fs::canonicalize(workspace_root).with_context(|| {
                format!(
                    "resolve Agent memory workspace {}",
                    workspace_root.display()
                )
            })?);
        }
        {
            let _writer_lock = acquire_writer_lock(&self.index_dir)?;
            self.catalog.upsert_agent_memory_source(&source)?;
        }
        self.index_root(&source.path, options)
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
        self.search_filtered(query, limit, &[], &[], None, None)
    }

    pub fn search_filtered(
        &self,
        query: &str,
        limit: usize,
        roots: &[PathBuf],
        kinds: &[String],
        path_prefix: Option<&str>,
        context_path: Option<&Path>,
    ) -> Result<Vec<SearchHit>> {
        if query.trim().is_empty() {
            anyhow::bail!("search query must not be empty");
        }
        let total_started_at = Instant::now();
        let _in_flight = self.metrics.begin();
        let registered_roots = self.catalog.roots()?;
        let mut root_filter = HashSet::new();
        let mut search_scopes = Vec::new();
        for root in roots {
            let canonical = fs::canonicalize(root)
                .with_context(|| format!("resolve search root {}", root.display()))?;
            search_scopes.push(canonical.clone());
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
        let context_path = context_path.map(resolve_context_path).transpose()?;
        if let Some(context_path) = &context_path
            && !registered_roots
                .iter()
                .any(|(_, root)| root.is_dir() && context_path.starts_with(root))
        {
            anyhow::bail!(
                "context path {} is outside every indexed workspace directory",
                context_path.display()
            );
        }
        let filtered = !root_filter.is_empty()
            || !kind_filter.is_empty()
            || path_prefix.is_some()
            || context_path.is_some();
        let overfetch = if filtered {
            limit.saturating_mul(20).clamp(200, 1_000)
        } else {
            limit.saturating_mul(10).max(100)
        };
        let include_agent_lane = context_path.is_some()
            || kind_filter
                .iter()
                .any(|kind| matches!(kind, FileKind::AgentInstructions | FileKind::AgentSkill));
        let include_memory = kind_filter.contains(&FileKind::AgentMemory);
        let generation = self.catalog.latest_completed_generation()?;
        let cache_key = serde_json::to_string(&(
            &self.index_dir,
            generation,
            query,
            limit,
            &search_scopes,
            kinds,
            path_prefix,
            &context_path,
        ))
        .context("encode search cache key")?;
        if let Some(output) = self.metrics.get_cached(&cache_key) {
            self.metrics.record(RetrievalDurations {
                total: total_started_at.elapsed(),
                lexical: Duration::ZERO,
                semantic: None,
                fusion: Duration::ZERO,
                catalog_filter: Duration::ZERO,
                preview: Duration::ZERO,
                semantic_fallback: false,
            });
            return Ok(output);
        }
        let semantic_index = &self.semantic;
        let semantic_path_prefixes = match path_prefix {
            Some(prefix) if Path::new(prefix).is_absolute() => vec![prefix.to_owned()],
            Some(prefix) => registered_roots
                .iter()
                .filter_map(|(_, root)| {
                    let root_text = root.to_string_lossy();
                    (root.is_dir()
                        && (root_filter.is_empty() || root_filter.contains(root_text.as_ref())))
                    .then(|| root.join(prefix).to_string_lossy().into_owned())
                })
                .collect(),
            None => Vec::new(),
        };
        let (lexical, semantic) = thread::scope(|scope| {
            let semantic_scopes = &search_scopes;
            let semantic_prefixes = &semantic_path_prefixes;
            let semantic = generation
                .filter(|_| semantic_index.enabled())
                .map(|generation| {
                    scope.spawn(move || {
                        let started_at = Instant::now();
                        let result = semantic_index.search(
                            query,
                            generation,
                            semantic_scopes,
                            kinds,
                            semantic_prefixes,
                        );
                        (result, started_at.elapsed())
                    })
                });
            let lexical_started_at = Instant::now();
            let lexical = (|| {
                let mut candidates =
                    self.search
                        .search(query, overfetch, overfetch, include_agent_lane)?;
                if include_memory {
                    candidates.extend(
                        self.memory_search
                            .search(query, overfetch, overfetch, false)?,
                    );
                    candidates.sort_by(|left, right| {
                        right
                            .score
                            .total_cmp(&left.score)
                            .then_with(|| left.path.cmp(&right.path))
                    });
                    let mut seen = HashSet::new();
                    candidates.retain(|candidate| seen.insert(candidate.file_id));
                    candidates.truncate(overfetch);
                }
                Ok::<_, anyhow::Error>(candidates)
            })();
            let lexical_duration = lexical_started_at.elapsed();
            let semantic = semantic.map(|worker| {
                worker.join().unwrap_or_else(|_| {
                    (
                        Err(anyhow::anyhow!("semantic search worker panicked")),
                        Duration::ZERO,
                    )
                })
            });
            ((lexical, lexical_duration), semantic)
        });
        let (lexical, lexical_duration) = lexical;
        let mut candidates = lexical?;
        let fusion_started_at = Instant::now();
        let mut semantic_duration = None;
        let mut semantic_fallback = false;
        if let Some(semantic) = semantic {
            semantic_duration = Some(semantic.1);
            match semantic {
                (Ok(semantic), _) => {
                    candidates = merge_semantic_candidates(
                        candidates,
                        semantic,
                        overfetch,
                        self.semantic.vector_weight(),
                    );
                }
                (Err(error), _) => {
                    semantic_fallback = true;
                    eprintln!("AWI semantic search unavailable; using lexical results: {error:#}");
                }
            }
        }
        let exact_symbols = self
            .catalog
            .exact_symbol_definitions(&query_symbol_terms(query))?;
        let mut exact_symbols_by_file = HashMap::new();
        for (file_id, path, generation, symbol) in exact_symbols {
            exact_symbols_by_file
                .entry(file_id)
                .or_insert_with(|| symbol.clone());
            if let Some(candidate) = candidates
                .iter_mut()
                .find(|candidate| candidate.file_id == file_id)
            {
                if !candidate.lanes.iter().any(|lane| lane == "exact_symbol") {
                    candidate.lanes.push("exact_symbol".to_owned());
                }
            } else {
                candidates.push(crate::model::SearchCandidate {
                    file_id,
                    path,
                    generation,
                    score: 0.0,
                    lanes: vec!["exact_symbol".to_owned()],
                });
            }
        }
        let fusion_duration = fusion_started_at.elapsed();
        let catalog_started_at = Instant::now();
        let candidate_ids = candidates
            .iter()
            .map(|candidate| candidate.file_id)
            .collect::<Vec<_>>();
        let files = self.catalog.current_files_by_ids(&candidate_ids)?;
        let agent_ids = files
            .values()
            .filter(|file| {
                matches!(
                    file.kind,
                    FileKind::AgentInstructions | FileKind::AgentSkill
                )
            })
            .map(|file| file.id)
            .collect::<Vec<_>>();
        let agents = self.catalog.agent_documents_for(&agent_ids)?;
        let memory_ids = files
            .values()
            .filter(|file| file.kind == FileKind::AgentMemory)
            .map(|file| file.id)
            .collect::<Vec<_>>();
        let memories = self.catalog.agent_memories_for(&memory_ids)?;
        let mut ranked_hits = Vec::with_capacity(limit);
        for candidate in candidates {
            let Some(file) = files.get(&candidate.file_id).cloned() else {
                continue;
            };
            if file.generation != candidate.generation
                || (!kind_filter.is_empty() && !kind_filter.contains(&file.kind))
                || (file.kind == FileKind::AgentMemory && !include_memory)
            {
                continue;
            }
            let agent = agents.get(&file.id).cloned();
            let memory = memories.get(&file.id).cloned();
            let applicable_instruction = context_path.as_ref().is_some_and(|context| {
                agent.as_ref().is_some_and(|metadata| {
                    metadata.role == AgentDocumentRole::Instructions
                        && context.starts_with(&metadata.scope_root)
                })
            });
            if context_path.is_some()
                && agent.as_ref().is_some_and(|metadata| {
                    metadata.role == AgentDocumentRole::Instructions && !applicable_instruction
                })
            {
                continue;
            }
            let applicable_memory = memory.as_ref().is_some_and(|metadata| {
                metadata.workspace_root.as_ref().is_none_or(|workspace| {
                    context_path
                        .as_ref()
                        .is_none_or(|context| context.starts_with(workspace))
                        && (search_scopes.is_empty()
                            || search_scopes.iter().any(|scope| {
                                workspace.starts_with(scope) || scope.starts_with(workspace)
                            }))
                })
            });
            if memory.is_some()
                && (!search_scopes.is_empty() || context_path.is_some())
                && !applicable_memory
            {
                continue;
            }
            if (!root_filter.is_empty() && !root_filter.contains(&file.root))
                && !applicable_instruction
                && !applicable_memory
            {
                continue;
            }
            if path_prefix.is_some_and(|prefix| {
                !file.absolute_path.starts_with(prefix) && !file.relative_path.starts_with(prefix)
            }) && !applicable_instruction
            {
                continue;
            }
            let mut score = candidate.score;
            if applicable_instruction {
                let depth = agent
                    .as_ref()
                    .map_or(0, |metadata| metadata.precedence_depth);
                score += APPLICABLE_INSTRUCTION_BOOST + depth as f32 * INSTRUCTION_DEPTH_BOOST;
            }
            if let Some(metadata) = &memory {
                score += if metadata.workspace_root.is_some() && applicable_memory {
                    EXACT_PROJECT_MEMORY_BOOST
                } else {
                    GLOBAL_MEMORY_BOOST
                };
                score += f32::from(metadata.layer.summary_priority()) * MEMORY_LAYER_BOOST;
                score += memory_recency_boost(metadata.observed_at_ms);
                let memory_match = memory_metadata_match(query, &file.name, metadata);
                score += memory_match.boost;
                if is_specific_query(query)
                    && metadata.layer == crate::model::AgentMemoryLayer::ProjectSummary
                    && !memory_match.exact_entity
                {
                    score -= AGGREGATE_MEMORY_PENALTY;
                }
            }
            let symbol = exact_symbols_by_file.get(&file.id).cloned();
            if symbol.is_some() {
                score += EXACT_SYMBOL_BOOST;
            }
            let content_hash = file.content_hash.clone();
            ranked_hits.push((
                SearchHit {
                    file_id: file.id,
                    path: file.absolute_path,
                    relative_path: file.relative_path,
                    kind: file.kind,
                    experiment: file.experiment,
                    size_bytes: file.size_bytes,
                    mtime_ns: file.mtime_ns,
                    generation: file.generation,
                    score,
                    matched_lanes: candidate.lanes,
                    preview: String::new(),
                    symbol,
                    agent,
                    memory,
                },
                content_hash,
            ));
        }
        ranked_hits.sort_by(|(left, _), (right, _)| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.path.cmp(&right.path))
        });
        let mut agent_hashes = HashSet::new();
        let mut memory_hashes = HashSet::new();
        ranked_hits.retain(|(hit, content_hash)| {
            if let Some(agent) = &hit.agent {
                return content_hash.as_ref().is_none_or(|hash| {
                    agent_hashes.insert((agent.role.as_str().to_owned(), hash.clone()))
                });
            }
            if hit.memory.is_some() {
                return content_hash
                    .as_ref()
                    .is_none_or(|hash| memory_hashes.insert(hash.clone()));
            }
            true
        });
        let mut ranked_hits = rerank_for_directory_diversity(ranked_hits, limit);
        let catalog_filter_duration = catalog_started_at.elapsed();
        let preview_started_at = Instant::now();
        self.focus_search_previews(query, &mut ranked_hits);
        let preview_duration = preview_started_at.elapsed();
        let output = ranked_hits
            .into_iter()
            .map(|(hit, _)| hit)
            .collect::<Vec<_>>();
        self.metrics.cache(cache_key, output.clone());
        self.metrics.record(RetrievalDurations {
            total: total_started_at.elapsed(),
            lexical: lexical_duration,
            semantic: semantic_duration,
            fusion: fusion_duration,
            catalog_filter: catalog_filter_duration,
            preview: preview_duration,
            semantic_fallback,
        });
        Ok(output)
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
        self.inspect_excerpt_inner(path.as_ref(), start_line, max_lines, max_chars, None)
    }

    pub fn inspect_symbol(
        &self,
        path: impl AsRef<Path>,
        symbol: &str,
        max_lines: usize,
        max_chars: usize,
    ) -> Result<InspectResult> {
        if symbol.trim().is_empty() {
            anyhow::bail!("inspect symbol must not be empty");
        }
        self.inspect_excerpt_inner(path.as_ref(), 1, max_lines, max_chars, Some(symbol))
    }

    fn inspect_excerpt_inner(
        &self,
        path: &Path,
        start_line: usize,
        max_lines: usize,
        max_chars: usize,
        symbol: Option<&str>,
    ) -> Result<InspectResult> {
        if start_line == 0 || max_lines == 0 || max_chars == 0 {
            anyhow::bail!("inspect excerpt limits must be positive");
        }
        let path =
            fs::canonicalize(path).with_context(|| format!("resolve path {}", path.display()))?;
        let absolute_path = path.to_string_lossy();
        let file = self
            .catalog
            .current_file_by_path(&absolute_path)?
            .with_context(|| {
                format!(
                    "path is not present in the current index: {absolute_path}; \
                     use workspace_search first and inspect only a returned path, or use \
                     ordinary file tools when search has no matching hit"
                )
            })?;
        let symbols = self.catalog.symbols_for(file.id)?;
        let focused_symbol = symbol
            .map(|requested| {
                symbols
                    .iter()
                    .filter(|candidate| candidate.name.eq_ignore_ascii_case(requested))
                    .min_by_key(|candidate| {
                        (
                            matches!(candidate.kind.as_str(), "call" | "import"),
                            candidate.line_start,
                        )
                    })
                    .cloned()
                    .with_context(|| {
                        format!("symbol {requested:?} is not indexed in {}", path.display())
                    })
            })
            .transpose()?;
        let dataset = self.catalog.dataset_profile_for(file.id)?;
        let agent = self.catalog.agent_document_for(file.id)?;
        let memory = self.catalog.agent_memory_for(file.id)?;
        let start_line = focused_symbol.as_ref().map_or(start_line, |symbol| {
            symbol.line_start.saturating_sub(2).max(1)
        });
        let content = content_excerpt(&path, &file, start_line, max_lines, max_chars)?;
        Ok(InspectResult {
            file,
            symbols,
            focused_symbol,
            dataset,
            agent,
            memory,
            content,
        })
    }

    pub fn status(&self) -> Result<IndexStatus> {
        let mut status = self.catalog.status()?;
        status.semantic = self.semantic.status();
        status.retrieval = self.metrics.snapshot();
        status.semantic.generation_lag =
            match (status.completed_generation, status.semantic.generation) {
                (Some(catalog), Some(semantic)) => catalog.saturating_sub(semantic) as u64,
                _ => 0,
            };
        if status.semantic.available && status.semantic.generation != status.completed_generation {
            status.semantic.available = false;
            status.semantic.error = Some(format!(
                "semantic generation {:?} does not match catalog generation {:?}",
                status.semantic.generation, status.completed_generation
            ));
        }
        status.memory.generation_lag = match (status.completed_generation, status.memory.generation)
        {
            (Some(catalog), Some(memory)) => catalog.saturating_sub(memory) as u64,
            _ => 0,
        };
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        for (path, indexed_mtime_ns) in self.catalog.agent_memory_file_states()? {
            let Ok(metadata) = fs::metadata(&path) else {
                status.memory.missing_files = status.memory.missing_files.saturating_add(1);
                continue;
            };
            let source_mtime_ns = mtime_ns(&metadata);
            let source_mtime_ms =
                u64::try_from(source_mtime_ns.saturating_div(1_000_000)).unwrap_or_default();
            status.memory.oldest_source_age_ms = status
                .memory
                .oldest_source_age_ms
                .max(now_ms.saturating_sub(source_mtime_ms));
            if source_mtime_ns > indexed_mtime_ns {
                status.memory.stale_files = status.memory.stale_files.saturating_add(1);
                let lag_ms = u64::try_from(
                    source_mtime_ns
                        .saturating_sub(indexed_mtime_ns)
                        .saturating_div(1_000_000),
                )
                .unwrap_or(u64::MAX);
                status.memory.max_source_lag_ms = status.memory.max_source_lag_ms.max(lag_ms);
            }
        }
        Ok(status)
    }

    pub fn warm_retrieval(&self) -> Result<()> {
        self.semantic.warm()?;
        self.search("warm workspace retrieval", 1)?;
        Ok(())
    }

    /// Canonical non-memory roots currently registered in the catalog,
    /// newest-nested first. The producer refreshes memory from its project
    /// registry before reconciling these roots.
    pub fn indexed_roots(&self) -> Result<Vec<PathBuf>> {
        self.catalog.publisher_roots()
    }

    /// Delete generation bookkeeping rows older than the latest completed
    /// generation so repeated reconciles cannot grow the catalog without
    /// bound. Returns the number of rows removed.
    pub fn prune_generation_history(&mut self) -> Result<usize> {
        let _writer_lock = acquire_writer_lock(&self.index_dir)?;
        let Some(latest) = self.catalog.latest_completed_generation()? else {
            return Ok(0);
        };
        self.catalog.prune_generation_rows(latest)
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

    pub fn rebuild_semantic(
        &mut self,
        options: &IndexOptions,
        resume: bool,
    ) -> Result<SemanticBuildReport> {
        if !self.semantic.enabled() {
            anyhow::bail!("semantic indexing requires AWI_SEMANTIC_MODEL");
        }
        let _writer_lock = acquire_writer_lock(&self.index_dir)?;
        let files = self.catalog.current_semantic_files()?;
        let root = self
            .catalog
            .roots()?
            .into_iter()
            .next()
            .map(|(_, root)| root)
            .context("cannot build semantics without an indexed root")?;
        let (root_id, generation) = self.catalog.start_generation(&root)?;
        let result = (|| {
            let existing = if resume {
                self.semantic.file_generations()?
            } else {
                self.semantic.reset()?;
                HashSet::new()
            };
            let rebuild_sql = resume && !self.semantic.sql_chunks_are_current()?;
            let mut report = SemanticBuildReport {
                generation,
                ..SemanticBuildReport::default()
            };
            let mut batch = Vec::new();
            let mut expected = Vec::with_capacity(files.len());
            for file in &files {
                let path = Path::new(&file.absolute_path);
                let text = extract_text(path, options.max_content_bytes)
                    .with_context(|| format!("extract semantic source {}", path.display()))?;
                if text.content_hash != file.content_hash {
                    anyhow::bail!(
                        "semantic source changed since catalog generation {}: {}; \
                         run reconcile first",
                        file.generation,
                        path.display()
                    );
                }
                let is_sql = path
                    .extension()
                    .and_then(OsStr::to_str)
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("sql"));
                if existing.contains(&(file.id, file.generation)) && !(rebuild_sql && is_sql) {
                    expected.push((file.id, file.generation));
                    report.files += 1;
                    report.reused_files += 1;
                    continue;
                }
                let symbols = self
                    .catalog
                    .symbols_for(file.id)?
                    .into_iter()
                    .flat_map(|symbol| [symbol.name, symbol.signature.unwrap_or_default()])
                    .collect::<Vec<_>>()
                    .join(" ");
                let schema = self
                    .catalog
                    .dataset_profile_for(file.id)?
                    .map(|profile| profile.schema_text())
                    .unwrap_or_default();
                expected.push((file.id, file.generation));
                batch.push(SearchDocument {
                    file_id: file.id,
                    path: file.absolute_path.clone(),
                    name: file.name.clone(),
                    experiment: file.experiment.clone().unwrap_or_default(),
                    kind: file.kind.as_str().to_owned(),
                    content: text.content.unwrap_or_default(),
                    symbols,
                    schema,
                    preview: String::new(),
                    generation: file.generation,
                });
                if batch.len() >= 128 {
                    let (indexed, chunks) = self.semantic.apply_changes(&batch, &[])?;
                    report.files += indexed;
                    report.chunks += chunks;
                    eprintln!(
                        "AWI semantic build progress: files={} reused={} chunks={}",
                        report.files, report.reused_files, report.chunks
                    );
                    batch.clear();
                }
            }
            if !batch.is_empty() {
                let (indexed, chunks) = self.semantic.apply_changes(&batch, &[])?;
                report.files += indexed;
                report.chunks += chunks;
            }
            self.semantic.seal(generation, &expected)?;
            Ok(report)
        })();
        match result {
            Ok(report) => {
                self.catalog.complete_generation(root_id, generation)?;
                Ok(report)
            }
            Err(error) => {
                let _ = self.catalog.record_failure(
                    generation,
                    "<semantic>",
                    "semantic_rebuild",
                    &format!("{error:#}"),
                );
                let _ = self
                    .catalog
                    .fail_generation(generation, &format!("{error:#}"));
                Err(error)
            }
        }
    }

    pub fn seal_semantic_generation(&self) -> Result<()> {
        let _writer_lock = acquire_writer_lock(&self.index_dir)?;
        let generation = self
            .catalog
            .latest_completed_generation()?
            .context("cannot seal semantics without a completed generation")?;
        let expected = self
            .catalog
            .current_semantic_files()?
            .into_iter()
            .map(|file| (file.id, file.generation))
            .collect::<Vec<_>>();
        self.semantic.seal(generation, &expected)
    }

    pub fn publish_snapshot(&mut self, publish_dir: impl AsRef<Path>) -> Result<SnapshotManifest> {
        let _writer_lock = acquire_writer_lock(&self.index_dir)?;
        let generation = self
            .catalog
            .latest_completed_generation()?
            .context("cannot publish an index without a completed generation")?;
        let expected = self
            .catalog
            .current_semantic_files()?
            .into_iter()
            .map(|file| (file.id, file.generation))
            .collect::<Vec<_>>();
        self.semantic.seal(generation, &expected)?;
        self.catalog.prune_generation_rows(generation)?;
        self.catalog.checkpoint()?;
        publish(
            &self.index_dir,
            publish_dir.as_ref(),
            generation,
            self.semantic.enabled(),
        )
    }

    fn apply_search_changes(
        &self,
        documents: &[SearchDocument],
        deleted_paths: &[String],
    ) -> Result<()> {
        let (memory, regular): (Vec<_>, Vec<_>) = documents
            .iter()
            .cloned()
            .partition(|document| document.kind == FileKind::AgentMemory.as_str());
        let mut regular_deleted = deleted_paths.to_vec();
        regular_deleted.extend(memory.iter().map(|document| document.path.clone()));
        regular_deleted.sort();
        regular_deleted.dedup();
        let mut memory_deleted = deleted_paths.to_vec();
        memory_deleted.extend(regular.iter().map(|document| document.path.clone()));
        memory_deleted.sort();
        memory_deleted.dedup();
        self.search.apply_changes(&regular, &regular_deleted)?;
        self.memory_search.apply_changes(&memory, &memory_deleted)
    }

    fn apply_semantic_changes(
        &self,
        generation: i64,
        documents: &[SearchDocument],
        deleted_paths: &[String],
    ) -> Result<(u64, u64)> {
        match self.semantic.apply_changes(documents, deleted_paths) {
            Ok(report) => Ok(report),
            Err(error) => {
                self.catalog.record_failure(
                    generation,
                    "<semantic>",
                    "semantic_index",
                    &format!("{error:#}"),
                )?;
                Err(error)
            }
        }
    }

    fn focus_search_previews(&self, query: &str, hits: &mut [(SearchHit, Option<String>)]) {
        let regular_ids = hits
            .iter()
            .filter(|(hit, _)| hit.kind != FileKind::AgentMemory)
            .map(|(hit, _)| hit.file_id)
            .collect::<Vec<_>>();
        let memory_ids = hits
            .iter()
            .filter(|(hit, _)| hit.kind == FileKind::AgentMemory)
            .map(|(hit, _)| hit.file_id)
            .collect::<Vec<_>>();
        let regular_previews = self
            .search
            .previews(query, &regular_ids, MATCH_PREVIEW_CHARS)
            .unwrap_or_default();
        let memory_previews = self
            .memory_search
            .previews(query, &memory_ids, MATCH_PREVIEW_CHARS)
            .unwrap_or_default();
        for (hit, _) in hits {
            if let Some(symbol) = &hit.symbol
                && let Some(preview) =
                    symbol_definition_preview(Path::new(&hit.path), symbol, MATCH_PREVIEW_CHARS)
            {
                hit.preview = preview;
                continue;
            }
            let previews = if hit.kind == FileKind::AgentMemory {
                &memory_previews
            } else {
                &regular_previews
            };
            if let Some(preview) = previews.get(&hit.file_id) {
                hit.preview.clone_from(preview);
            }
        }
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

        if root.is_file() {
            self.index_discovered_file(
                root,
                root_id,
                generation,
                root,
                options,
                &profiler,
                &mut documents,
                &mut report,
            )?;
        } else {
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
                if entry
                    .file_type()
                    .is_some_and(|file_type| file_type.is_file())
                    && !is_sensitive(entry.path())
                {
                    self.index_discovered_file(
                        root,
                        root_id,
                        generation,
                        entry.path(),
                        options,
                        &profiler,
                        &mut documents,
                        &mut report,
                    )?;
                }
            }
        }

        let deleted_paths = self.catalog.mark_missing_deleted(root_id, generation)?;
        report.deleted = deleted_paths.len() as u64;
        self.apply_semantic_changes(generation, &documents, &deleted_paths)?;
        self.apply_search_changes(&documents, &deleted_paths)?;
        self.catalog.complete_generation(root_id, generation)?;
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    fn index_discovered_file(
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
        report.discovered += 1;
        if let Err(error) = self.index_file(
            root, root_id, generation, path, options, profiler, documents, report,
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
        Ok(())
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

        self.apply_semantic_changes(generation, &documents, &deleted_paths)?;
        self.apply_search_changes(&documents, &deleted_paths)?;
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
        let memory_source = self.catalog.agent_memory_source_for_path(path)?;
        let kind = if memory_source.is_some() {
            FileKind::AgentMemory
        } else {
            classify(path)
        };
        let absolute_path = path.to_string_lossy().into_owned();
        let relative_path = if root.is_file() {
            path.file_name()
                .and_then(OsStr::to_str)
                .unwrap_or_default()
                .to_owned()
        } else {
            path.strip_prefix(root)
                .with_context(|| format!("strip root from {}", path.display()))?
                .to_string_lossy()
                .into_owned()
        };
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default()
            .to_owned();
        let experiment = experiment_name(Path::new(&relative_path));
        let existing = self.catalog.existing_file(&absolute_path)?;
        let existing_is_current = existing
            .as_ref()
            .is_some_and(|state| state.generation_completed);

        if existing_is_current
            && existing.as_ref().is_some_and(|state| {
                state.size_bytes == size_bytes && state.mtime_ns == mtime_ns && state.kind == kind
            })
        {
            let file_id = existing.as_ref().expect("checked above").id;
            self.catalog
                .mark_seen(file_id, generation, size_bytes, mtime_ns)?;
            report.unchanged += 1;
            return Ok(());
        }

        let mut text = if should_extract_text(kind, size_bytes, options.max_content_bytes) {
            extract_text(path, options.max_content_bytes)?
        } else {
            crate::extract::TextExtraction {
                content: None,
                content_hash: None,
                preview: String::new(),
                status: "metadata_only".to_owned(),
            }
        };
        if matches!(
            kind,
            FileKind::AgentInstructions | FileKind::AgentSkill | FileKind::AgentMemory
        ) && text
            .content
            .as_deref()
            .is_some_and(contains_sensitive_agent_content)
        {
            text.content = None;
            text.content_hash = None;
            text.preview.clear();
            text.status = "metadata_only_sensitive_content".to_owned();
        }
        if kind == FileKind::AgentMemory
            && let Some(content) = text.content.take()
        {
            let normalized = normalize_agent_memory_content(
                path,
                &content,
                memory_source
                    .as_ref()
                    .is_some_and(|source| source.raw_history),
            );
            text.preview = index_preview(&normalized);
            text.content = Some(normalized);
        }

        if existing_is_current
            && existing.as_ref().is_some_and(|state| {
                state.content_hash.is_some()
                    && state.content_hash == text.content_hash
                    && state.kind == kind
            })
        {
            let file_id = existing.as_ref().expect("checked above").id;
            self.catalog
                .mark_seen(file_id, generation, size_bytes, mtime_ns)?;
            report.unchanged += 1;
            return Ok(());
        }

        let (symbols, code_status) = extract_code(path, kind, text.content.as_deref())?;
        let (agent, agent_status) = if let Some(content) = text.content.as_deref()
            && matches!(kind, FileKind::AgentInstructions | FileKind::AgentSkill)
        {
            match extract_agent_document(root, path, content) {
                Ok(agent) => (agent, Some("agent_parsed")),
                Err(error) => {
                    report.failed += 1;
                    self.catalog.record_failure(
                        generation,
                        &absolute_path,
                        "agent_metadata",
                        &format!("{error:#}"),
                    )?;
                    (None, Some("agent_parse_failed"))
                }
            }
        } else {
            (None, None)
        };
        let memory = memory_source
            .as_ref()
            .map(|source| memory_metadata(source, path, text.content.as_deref(), mtime_ns));
        let memory_status = memory.as_ref().map(|_| "memory_parsed");
        let profile = (kind != FileKind::AgentMemory)
            .then(|| profiler.profile(path, size_bytes))
            .flatten();
        let extraction_status = combined_status(
            &text.status,
            &code_status,
            profile.as_ref(),
            agent_status,
            memory_status,
        );
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
        self.catalog
            .replace_agent_document(file_id, agent.as_ref())?;
        self.catalog
            .replace_agent_memory(file_id, memory.as_ref())?;

        let mut symbols_text = symbols
            .iter()
            .flat_map(|symbol| {
                [
                    symbol.name.as_str(),
                    symbol.signature.as_deref().unwrap_or(""),
                ]
            })
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(agent) = &agent {
            if !symbols_text.is_empty() {
                symbols_text.push(' ');
            }
            symbols_text.push_str(&agent.search_text());
        }
        if let Some(memory) = &memory {
            if !symbols_text.is_empty() {
                symbols_text.push(' ');
            }
            symbols_text.push_str(&memory.search_text());
        }
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

fn resolve_context_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    fs::canonicalize(&absolute)
        .with_context(|| format!("resolve Agent context path {}", absolute.display()))
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
    agent_status: Option<&str>,
    memory_status: Option<&str>,
) -> String {
    let mut parts = vec![text_status, code_status];
    if let Some(profile) = profile {
        parts.push(&profile.status);
    }
    if let Some(agent_status) = agent_status {
        parts.push(agent_status);
    }
    if let Some(memory_status) = memory_status {
        parts.push(memory_status);
    }
    parts.join(",")
}

fn query_symbol_terms(query: &str) -> Vec<String> {
    let raw = query
        .split_whitespace()
        .map(|term| {
            term.trim_matches(|ch: char| !(ch.is_alphanumeric() || matches!(ch, '_' | ':' | '.')))
                .trim_end_matches("()")
        })
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    let single_term = raw.len() == 1;
    let mut seen = HashSet::new();
    raw.into_iter()
        .filter(|term| {
            term.len() >= 2
                && (single_term
                    || term.contains('_')
                    || term.contains("::")
                    || term.chars().skip(1).any(|ch| ch.is_ascii_uppercase())
                    || term.chars().any(|ch| ch.is_ascii_digit()))
        })
        .map(str::to_owned)
        .filter(|term| seen.insert(term.to_ascii_lowercase()))
        .collect()
}

fn symbol_definition_preview(
    path: &Path,
    symbol: &SymbolRecord,
    max_chars: usize,
) -> Option<String> {
    let source = fs::read_to_string(path).ok()?;
    let start = symbol.line_start.saturating_sub(2).max(1);
    let end = symbol.line_end.saturating_add(8);
    let excerpt = source
        .lines()
        .enumerate()
        .filter(|(index, _)| {
            let line = index + 1;
            line >= start && line <= end
        })
        .map(|(_, line)| line)
        .collect::<Vec<_>>()
        .join("\n");
    let evidence = format!(
        "{} {} at lines {}-{}{}",
        symbol.kind,
        symbol.name,
        symbol.line_start,
        symbol.line_end,
        symbol
            .signature
            .as_deref()
            .map(|signature| format!(": {signature}"))
            .unwrap_or_default()
    );
    Some(
        format!("{evidence}\n\n{excerpt}")
            .chars()
            .take(max_chars)
            .collect(),
    )
}

fn memory_recency_boost(observed_at_ms: i64) -> f32 {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or(observed_at_ms);
    let age_days = now_ms.saturating_sub(observed_at_ms).max(0) as f32 / 86_400_000.0;
    MAX_MEMORY_RECENCY_BOOST / (1.0 + age_days / 30.0)
}

#[derive(Debug, Clone, Copy, Default)]
struct MemoryMetadataMatch {
    boost: f32,
    exact_entity: bool,
}

fn memory_metadata_match(
    query: &str,
    filename: &str,
    metadata: &crate::model::AgentMemoryMetadata,
) -> MemoryMetadataMatch {
    let query = normalized_match_text(query);
    let filename = Path::new(filename)
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or(filename);
    let filename = normalized_match_text(filename);
    let name = metadata.name.as_deref().map(normalized_match_text);
    let filename_match = filename.len() >= 4 && contains_entity(&query, &filename);
    let name_match = name
        .as_deref()
        .is_some_and(|name| name.len() >= 4 && contains_entity(&query, name));
    let mut boost = 0.0;
    if filename_match {
        boost += EXACT_MEMORY_FILENAME_BOOST;
    }
    if name_match {
        boost += EXACT_MEMORY_NAME_BOOST;
    }
    if let Some(description) = metadata.description.as_deref() {
        let description = normalized_match_text(description);
        let query_terms = query.split_whitespace().collect::<HashSet<_>>();
        let description_terms = description.split_whitespace().collect::<HashSet<_>>();
        let matched = query_terms.intersection(&description_terms).count();
        if matched >= 2 || matched == 1 && query_terms.len() == 1 {
            let ratio = matched as f32 / query_terms.len().max(1) as f32;
            boost += MEMORY_DESCRIPTION_BOOST * ratio;
        }
    }
    MemoryMetadataMatch {
        boost,
        exact_entity: filename_match || name_match,
    }
}

fn normalized_match_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn contains_entity(query: &str, entity: &str) -> bool {
    query == entity
        || query
            .strip_prefix(entity)
            .is_some_and(|suffix| suffix.starts_with(' '))
        || query
            .strip_suffix(entity)
            .is_some_and(|prefix| prefix.ends_with(' '))
        || query.contains(&format!(" {entity} "))
}

fn is_specific_query(query: &str) -> bool {
    let normalized = normalized_match_text(query);
    normalized
        .split_whitespace()
        .any(|term| term.len() >= 16 || term.chars().any(|ch| ch.is_ascii_digit()))
        || query
            .split_whitespace()
            .any(has_internal_identifier_separator)
}

fn has_internal_identifier_separator(term: &str) -> bool {
    let chars = term.chars().collect::<Vec<_>>();
    chars.windows(3).any(|window| {
        window[0].is_alphanumeric()
            && matches!(window[1], '-' | '_' | '/' | '.')
            && window[2].is_alphanumeric()
    })
}

fn rerank_for_directory_diversity(
    mut candidates: Vec<(SearchHit, Option<String>)>,
    limit: usize,
) -> Vec<(SearchHit, Option<String>)> {
    let mut selected = Vec::with_capacity(limit.min(candidates.len()));
    let mut selected_parents = HashSet::new();
    while selected.len() < limit && !candidates.is_empty() {
        let mut best_index = 0;
        for index in 1..candidates.len() {
            let candidate = &candidates[index].0;
            let best = &candidates[best_index].0;
            let candidate_score = candidate.score
                - if selected_parents.contains(parent_path(&candidate.path)) {
                    SAME_DIRECTORY_MMR_PENALTY
                } else {
                    0.0
                };
            let best_score = best.score
                - if selected_parents.contains(parent_path(&best.path)) {
                    SAME_DIRECTORY_MMR_PENALTY
                } else {
                    0.0
                };
            if candidate_score > best_score
                || (candidate_score == best_score && candidate.path < best.path)
            {
                best_index = index;
            }
        }
        let mut candidate = candidates.swap_remove(best_index);
        let parent = parent_path(&candidate.0.path).to_owned();
        if selected_parents.contains(&parent) {
            candidate.0.score -= SAME_DIRECTORY_MMR_PENALTY;
        }
        selected_parents.insert(parent);
        selected.push(candidate);
    }
    selected
}

fn parent_path(path: &str) -> &Path {
    Path::new(path).parent().unwrap_or_else(|| Path::new(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranked_hit(path: &str, score: f32) -> (SearchHit, Option<String>) {
        (
            SearchHit {
                file_id: 1,
                path: path.to_owned(),
                relative_path: path.to_owned(),
                kind: FileKind::Text,
                experiment: None,
                size_bytes: 0,
                mtime_ns: 0,
                generation: 1,
                score,
                matched_lanes: vec!["content".to_owned()],
                preview: String::new(),
                symbol: None,
                agent: None,
                memory: None,
            },
            None,
        )
    }

    #[test]
    fn directory_diversity_prevents_one_folder_from_filling_results() {
        let candidates = vec![
            ranked_hit("/workspace/a/first.json", 0.100),
            ranked_hit("/workspace/a/second.json", 0.096),
            ranked_hit("/workspace/b/implementation.sql", 0.095),
        ];

        let selected = rerank_for_directory_diversity(candidates, 2);
        assert_eq!(selected[0].0.path, "/workspace/a/first.json");
        assert_eq!(selected[1].0.path, "/workspace/b/implementation.sql");
    }

    #[test]
    fn semantic_coverage_excludes_zero_byte_files() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("workspace");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("empty.rs"), "").unwrap();
        fs::write(root.join("present.rs"), "pub fn present() {}\n").unwrap();

        let mut workspace = WorkspaceIndex::open(fixture.path().join("index")).unwrap();
        workspace
            .index_root(&root, &IndexOptions::default())
            .unwrap();
        let files = workspace.catalog.current_semantic_files().unwrap();
        let names = files.into_iter().map(|file| file.name).collect::<Vec<_>>();
        assert_eq!(names, vec!["present.rs"]);
    }

    #[test]
    fn read_only_open_does_not_fail_an_active_generation() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().join("workspace");
        fs::create_dir_all(&root).unwrap();
        let index_dir = fixture.path().join("index");
        let mut writer = WorkspaceIndex::open(&index_dir).unwrap();
        let (_, generation) = writer.catalog.start_generation(&root).unwrap();

        let observer = WorkspaceIndex::open(&index_dir).unwrap();
        let status = observer.status().unwrap();
        assert_eq!(status.running_generations, 1);
        assert_eq!(status.failed_generations, 0);

        let (_, replacement) = writer.catalog.start_generation(&root).unwrap();
        let status = writer.status().unwrap();
        assert_eq!(status.running_generations, 1);
        assert_eq!(status.failed_generations, 1);
        writer
            .catalog
            .fail_generation(replacement, "test cleanup")
            .unwrap();
        assert!(replacement > generation);
    }
}
