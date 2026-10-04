use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs4::fs_std::FileExt;

use crate::collection::{LocalCollection, Visibility};
use crate::model::{IndexStatus, ShardStatus};
use crate::{
    IndexOptions, IndexReport, InspectResult, NotifyReport, QueryRequest, QueryResult, SearchHit,
    WorkspaceIndex,
};

struct Shard {
    name: String,
    workspace: WorkspaceIndex,
    visibility: Visibility,
}

/// One request holds one complete set of per-shard readers. Each query worker
/// owns its SQLite connections but shares semantic runtimes with its siblings.
pub(crate) struct ReaderIndex {
    path: PathBuf,
    shards: Vec<Shard>,
    collection: bool,
    _lease: File,
    /// Bounded cache for the heavy full-catalog status scan. Without this, every
    /// `status` request re-scans every shard's catalog; under concurrent load the
    /// scan exceeds the daemon IO budget and clients see `os error 11`. We serve
    /// a recently-computed status instead and refresh at most once per TTL.
    status_cache: std::sync::Mutex<Option<(std::time::Instant, IndexStatus)>>,
}

const STATUS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(2);

impl ReaderIndex {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_previous(path.as_ref(), None)
    }

    fn open_with_previous(path: &Path, previous: Option<&Self>) -> Result<Self> {
        fs::create_dir_all(path)?;
        let lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.join(".reader.lock"))?;
        FileExt::lock_shared(&lease)?;
        let descriptor = match fs::read(path.join("collection.local.json")) {
            Ok(bytes) => Some(serde_json::from_slice::<LocalCollection>(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let collection = descriptor.is_some();
        let definitions = match descriptor {
            Some(descriptor) => descriptor
                .shards
                .into_iter()
                .map(|shard| {
                    (
                        shard.definition.name,
                        shard.path,
                        shard.definition.visibility,
                    )
                })
                .collect(),
            None => vec![("default".to_owned(), path.to_owned(), Visibility::default())],
        };
        let mut shards = Vec::new();
        for (name, shard_path, visibility) in definitions {
            let old = previous.and_then(|old| old.shards.iter().find(|shard| shard.name == name));
            let mut workspace = match old {
                Some(old) => old.workspace.open_reader_at(&shard_path)?,
                None => WorkspaceIndex::open(&shard_path)?,
            };
            workspace.set_visibility(&visibility);
            shards.push(Shard {
                name,
                workspace,
                visibility,
            });
        }
        if shards.is_empty() {
            anyhow::bail!("reader collection is empty");
        }
        Ok(Self {
            path: path.to_owned(),
            shards,
            collection,
            _lease: lease,
            status_cache: std::sync::Mutex::new(None),
        })
    }

    pub(crate) fn open_reader_at(&self, path: &Path) -> Result<Self> {
        Self::open_with_previous(path, Some(self))
    }

    pub(crate) fn index_dir(&self) -> &Path {
        &self.path
    }

    pub(crate) fn warm_retrieval(&self) -> Result<()> {
        for shard in &self.shards {
            shard.workspace.warm_retrieval()?;
        }
        Ok(())
    }

    fn writer(&mut self) -> Result<&mut WorkspaceIndex> {
        if self.collection {
            anyhow::bail!("collection readers are read-only");
        }
        Ok(&mut self.shards[0].workspace)
    }

    pub(crate) fn index_root(
        &mut self,
        root: PathBuf,
        options: &IndexOptions,
    ) -> Result<IndexReport> {
        self.writer()?.index_root(root, options)
    }

    pub(crate) fn notify_paths(
        &mut self,
        paths: &[PathBuf],
        options: &IndexOptions,
    ) -> Result<NotifyReport> {
        self.writer()?.notify_paths(paths, options)
    }

    fn roots(&self) -> Result<Vec<PathBuf>> {
        let mut roots = Vec::new();
        for shard in &self.shards {
            roots.extend(shard.workspace.registered_roots()?);
        }
        roots.sort();
        roots.dedup();
        Ok(roots)
    }

    pub(crate) fn search_filtered(
        &mut self,
        query: &str,
        limit: usize,
        roots: &[PathBuf],
        kinds: &[String],
        path_prefix: Option<&str>,
        context_path: Option<&Path>,
    ) -> Result<Vec<SearchHit>> {
        Ok(self
            .search_filtered_stages(query, limit, roots, kinds, path_prefix, context_path)?
            .0)
    }

    /// Search plus the per-query stage bundle for the audit layer.
    pub(crate) fn search_filtered_stages(
        &mut self,
        query: &str,
        limit: usize,
        roots: &[PathBuf],
        kinds: &[String],
        path_prefix: Option<&str>,
        context_path: Option<&Path>,
    ) -> Result<(Vec<SearchHit>, serde_json::Value)> {
        if !self.collection {
            return self.shards[0].workspace.search_filtered_stages(
                query,
                limit,
                roots,
                kinds,
                path_prefix,
                context_path,
            );
        }
        let registered = self.roots()?;
        for root in roots {
            let canonical = fs::canonicalize(root)?;
            if !registered.iter().any(|path| {
                path.starts_with(&canonical) || (path.is_dir() && canonical.starts_with(path))
            }) {
                anyhow::bail!(
                    "search root {} is outside every indexed workspace root and parent scope",
                    root.display()
                );
            }
        }
        if let Some(context) = context_path {
            let context = crate::workspace::resolve_context_path(context)?;
            if !registered
                .iter()
                .any(|root| context.starts_with(root) && root.is_dir())
            {
                anyhow::bail!(
                    "context path {} is outside every indexed workspace directory",
                    context.display()
                );
            }
        }
        let batches = std::thread::scope(|scope| {
            let tasks: Vec<_> = self
                .shards
                .iter_mut()
                .map(|shard| {
                    scope.spawn(move || {
                        shard.workspace.search_in_scope_stages(
                            query,
                            // Keep the global evidence window independent of requested K.
                            200,
                            roots,
                            kinds,
                            path_prefix,
                            context_path,
                            true,
                        )
                    })
                })
                .collect();
            tasks
                .into_iter()
                .map(|task| {
                    task.join()
                        .map_err(|_| anyhow::anyhow!("shard search panicked"))?
                })
                .collect::<Result<Vec<_>>>()
        })?;
        // Per-query stages: shard searches run in parallel; the sidecar
        // diagnostics are per-process and identical across shards, so carry the
        // first non-trivial bundle.
        let stages = batches
            .iter()
            .find(|(_, stages)| {
                stages.get("embedding_ms").is_some() || stages.get("retrieval_cache_hit").is_some()
            })
            .map(|(_, stages)| stages.clone())
            .unwrap_or_else(|| serde_json::json!({"semantic_available": false}));
        let mut hits: Vec<_> = batches.into_iter().flat_map(|(hits, _)| hits).collect();
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.path.cmp(&b.path))
        });
        let mut paths = HashSet::new();
        hits.retain(|hit| paths.insert(hit.path.clone()));
        // RRF ranks must be computed across the visible candidate union.
        // Summing already-fused shard scores promotes every shard's local
        // rank-one result, and small shards can crowd out stronger evidence.
        let mut lanes = HashMap::<String, Vec<(usize, f32, f32)>>::new();
        for (index, hit) in hits.iter_mut().enumerate() {
            for evidence in &hit.evidence {
                hit.score -= evidence.contribution;
                lanes.entry(evidence.name.clone()).or_default().push((
                    index,
                    evidence.score,
                    evidence.weight,
                ));
            }
        }
        for candidates in lanes.values_mut() {
            candidates.sort_by(|a, b| {
                b.1.total_cmp(&a.1)
                    .then_with(|| hits[a.0].path.cmp(&hits[b.0].path))
            });
            for (rank, &(index, _, weight)) in candidates.iter().enumerate() {
                hits[index].score += weight / (61.0 + rank as f32);
            }
        }
        let ranked = crate::workspace::rerank_collection_diversity(
            hits.into_iter().map(|hit| (hit, None)).collect(),
            limit,
        )
        .into_iter()
        .map(|(hit, _)| hit)
        .collect();
        Ok((ranked, stages))
    }

    pub(crate) fn diagnose_search(
        &self,
        anchors: &[String],
        roots: &[PathBuf],
        context: Option<&Path>,
    ) -> Result<serde_json::Value> {
        crate::diagnostics::diagnose(anchors, roots, context, &self.roots()?, |path| {
            for shard in &self.shards {
                if let Some(file) = shard.workspace.diagnostic_file(path)? {
                    return Ok(Some(file));
                }
            }
            Ok(None)
        })
    }

    fn inspect_with(
        &self,
        path: &Path,
        inspect: impl Fn(&WorkspaceIndex) -> Result<InspectResult>,
    ) -> Result<InspectResult> {
        let path = crate::workspace::resolve_context_path(path)?;
        let mut last_error = None;
        for shard in &self.shards {
            if shard
                .visibility
                .exclude_paths
                .iter()
                .any(|prefix| path.starts_with(prefix))
            {
                continue;
            }
            match inspect(&shard.workspace) {
                Ok(result)
                    if shard.visibility.allows(
                        Path::new(&result.file.absolute_path),
                        result.file.kind.as_str(),
                    ) =>
                {
                    return Ok(result);
                }
                Ok(_) => {}
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            anyhow::anyhow!(
                "path is not indexed by an authoritative shard: {}",
                path.display()
            )
        }))
    }

    pub(crate) fn inspect_excerpt(
        &self,
        path: &Path,
        start: usize,
        lines: usize,
        chars: usize,
    ) -> Result<InspectResult> {
        self.inspect_with(path, |workspace| {
            workspace.inspect_excerpt(path, start, lines, chars)
        })
    }

    pub(crate) fn inspect_symbol(
        &self,
        path: &Path,
        symbol: &str,
        lines: usize,
        chars: usize,
    ) -> Result<InspectResult> {
        self.inspect_with(path, |workspace| {
            workspace.inspect_symbol(path, symbol, lines, chars)
        })
    }

    pub(crate) fn query(&self, request: &QueryRequest) -> Result<QueryResult> {
        let registered = self.roots()?;
        for root in &request.roots {
            let canonical = fs::canonicalize(root)
                .with_context(|| format!("resolve query root {}", root.display()))?;
            if !registered.contains(&canonical) {
                anyhow::bail!(
                    "query root {} is not an indexed workspace root",
                    root.display()
                );
            }
        }
        crate::data::execute_query(request)
    }

    pub(crate) fn status(&self) -> Result<IndexStatus> {
        if let Ok(guard) = self.status_cache.lock()
            && let Some((at, cached)) = guard.as_ref()
            && at.elapsed() < STATUS_CACHE_TTL
        {
            return Ok(cached.clone());
        }
        let computed = self.compute_status()?;
        if let Ok(mut guard) = self.status_cache.lock() {
            *guard = Some((std::time::Instant::now(), computed.clone()));
        }
        Ok(computed)
    }

    fn compute_status(&self) -> Result<IndexStatus> {
        if !self.collection {
            return self.shards[0].workspace.status();
        }
        let mut result = IndexStatus::default();
        for shard in &self.shards {
            let status = shard.workspace.status()?;
            result.active_files += status.active_files;
            result.deleted_files += status.deleted_files;
            result.symbols += status.symbols;
            result.datasets += status.datasets;
            result.failures += status.failures;
            result.running_generations += status.running_generations;
            result.failed_generations += status.failed_generations;
            result.agent_documents += status.agent_documents;
            if !shard
                .visibility
                .exclude_kinds
                .iter()
                .any(|kind| kind == "agent_memory")
            {
                result.agent_memories += status.agent_memories;
                result.memory.registered_projects += status.memory.registered_projects;
                result.memory.registered_sources += status.memory.registered_sources;
                result.memory.active_files += status.memory.active_files;
                result.memory.stale_files += status.memory.stale_files;
                result.memory.missing_files += status.memory.missing_files;
                result.memory.max_source_lag_ms = result
                    .memory
                    .max_source_lag_ms
                    .max(status.memory.max_source_lag_ms);
                result.memory.oldest_source_age_ms = result
                    .memory
                    .oldest_source_age_ms
                    .max(status.memory.oldest_source_age_ms);
            }
            result.shards.push(ShardStatus {
                name: shard.name.clone(),
                index_dir: shard.workspace.index_dir().to_owned(),
                visibility: shard.visibility.clone(),
                status,
            });
        }
        // Generations and latency distributions are shard-local; never invent
        // a comparable global generation or merge percentiles arithmetically.
        // Freshness aggregates the stalest shard (max age) as the system-level
        // number, while every shard's own publish timestamp/generation is preserved
        // in the per-shard maps. Generation counters across shards are NOT comparable
        // numbers, so the system generation is the generation OF the stalest shard
        // (the one whose publication time is oldest), and we name that shard — never
        // a min/max over the counters.
        let mut shard_last = std::collections::BTreeMap::new();
        let mut shard_generation = std::collections::BTreeMap::new();
        let mut oldest_age: Option<u64> = None;
        let mut oldest_shard: Option<String> = None;
        let mut scan_completed: Option<u64> = None;
        for entry in &result.shards {
            if let Some(ms) = entry.status.freshness.last_published_ms {
                shard_last.insert(entry.name.clone(), ms);
            }
            if let Some(published_gen) = entry.status.freshness.published_generation {
                shard_generation.insert(entry.name.clone(), published_gen);
            }
            if let Some(ms) = entry.status.freshness.scan_completed_ms {
                scan_completed = Some(scan_completed.map_or(ms, |acc| acc.max(ms)));
            }
            if let Some(age) = entry.status.freshness.age_ms {
                if oldest_age.map_or(true, |acc| age > acc) {
                    oldest_age = Some(age);
                    oldest_shard = Some(entry.name.clone());
                }
            }
        }
        result.freshness.shard_last_published_ms = shard_last;
        result.freshness.shard_published_generation = shard_generation.clone();
        result.freshness.age_ms = oldest_age;
        result.freshness.now_ms = result
            .shards
            .first()
            .map(|s| s.status.freshness.now_ms)
            .unwrap_or_default();
        result.freshness.last_published_ms =
            oldest_age.map(|oldest_age| result.freshness.now_ms.saturating_sub(oldest_age));
        // System generation = the generation of the stalest shard; name the shard so
        // it is auditable. Fall back to None when no shard published metadata.
        result.freshness.published_generation = oldest_shard
            .as_ref()
            .and_then(|name| shard_generation.get(name).copied());
        result.freshness.published_generation_shard = oldest_shard;
        result.freshness.scan_completed_ms = scan_completed;
        result.freshness.heartbeat_ms = None;
        // Aggregate the top-level semantic view across shards. The refresher reader
        // shares the warm serving sidecar Arc, so `runtime_ready` reflects the real
        // query lane. Counters are summed; latency WINDOWS are NOT merged (percentiles
        // cannot be added) — per-shard windows live in shards[].status.semantic.metrics,
        // and the top-level windows stay empty rather than picking one shard as truth.
        {
            let mut agg = crate::model::SemanticStatus::default();
            let mut queries = 0u64;
            let mut result_cache_hits = 0u64;
            let mut embedding_cache_hits = 0u64;
            let mut coalesced = 0u64;
            let mut failures = 0u64;
            let mut starts = 0u64;
            let mut restarts = 0u64;
            let mut startup_failures = 0u64;
            let mut snapshot_opened = 0u64;
            for entry in &result.shards {
                let s = &entry.status.semantic;
                agg.enabled |= s.enabled;
                agg.available |= s.available;
                if s.runtime_ready && !agg.runtime_ready {
                    agg.runtime_ready = true;
                    agg.generation = s.generation;
                    agg.files = s.files;
                    agg.chunks = s.chunks;
                    agg.chunking_version = s.chunking_version;
                }
                startup_failures = startup_failures.saturating_add(s.startup_failures);
                queries = queries.saturating_add(s.metrics.queries);
                result_cache_hits = result_cache_hits.saturating_add(s.metrics.result_cache_hits);
                embedding_cache_hits =
                    embedding_cache_hits.saturating_add(s.metrics.embedding_cache_hits);
                coalesced = coalesced.saturating_add(s.metrics.coalesced_queries);
                failures = failures.saturating_add(s.metrics.failures);
                starts = starts.saturating_add(s.metrics.sidecar_starts);
                restarts = restarts.saturating_add(s.metrics.sidecar_restarts);
                startup_failures = startup_failures.saturating_add(s.metrics.startup_failures);
                snapshot_opened = snapshot_opened.saturating_add(s.metrics.snapshot_opened);
            }
            agg.effective_mode = if agg.runtime_ready {
                "semantic".to_owned()
            } else {
                "lexical_only".to_owned()
            };
            agg.fallback_reason = if agg.runtime_ready {
                None
            } else {
                result
                    .shards
                    .iter()
                    .find_map(|e| e.status.semantic.fallback_reason.clone())
            };
            agg.startup_failures = startup_failures;
            agg.metrics.queries = queries;
            agg.metrics.result_cache_hits = result_cache_hits;
            agg.metrics.embedding_cache_hits = embedding_cache_hits;
            agg.metrics.coalesced_queries = coalesced;
            agg.metrics.failures = failures;
            agg.metrics.sidecar_starts = starts;
            agg.metrics.sidecar_restarts = restarts;
            agg.metrics.startup_failures = startup_failures;
            agg.metrics.snapshot_opened = snapshot_opened;
            result.semantic = agg;
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::QueryInput;
    use crate::collection::{Collection, ShardSource, refresh};
    use crate::snapshot::{read_pointer, write_json_sync};
    use tempfile::tempdir;

    #[test]
    fn federation_filters_before_top_k_and_prevents_deleted_path_fallback() {
        let fixture = tempdir().unwrap();
        let root = fixture.path().join("workspace");
        let data = fixture.path().join("data");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&data).unwrap();
        let file = root.join("current.rs");
        fs::write(&file, "fn federationneedle() {}\n").unwrap();
        fs::write(data.join("kept.rs"), "fn federationneedle() {}\n").unwrap();
        fs::write(data.join("rows.csv"), "value\n7\n").unwrap();
        fs::write(root.join("rows.csv"), "value\n5\n").unwrap();
        let child = root.join("child");
        fs::create_dir(&child).unwrap();
        fs::write(child.join("chosen.txt"), "scopedneedle").unwrap();
        fs::write(root.join("sibling.txt"), "scopedneedle").unwrap();
        fs::write(data.join("other.txt"), "scopedneedle").unwrap();
        // These higher-ranked old copies must not exhaust the base shard's
        // bounded candidate window before ownership filtering.
        for number in 0..210 {
            fs::write(
                root.join(format!("old{number}.rs")),
                "fn federationneedle() {}\n",
            )
            .unwrap();
        }
        let source = fixture.path().join("published");
        let base_source = source.join("base");
        let fast_source = source.join("fast");
        let mut base = WorkspaceIndex::open(fixture.path().join("base-index")).unwrap();
        base.index_root(&root, &IndexOptions::default()).unwrap();
        base.index_root(&data, &IndexOptions::default()).unwrap();
        base.publish_snapshot(&base_source).unwrap();
        for number in 0..210 {
            fs::remove_file(root.join(format!("old{number}.rs"))).unwrap();
        }
        let mut fast = WorkspaceIndex::open(fixture.path().join("fast-index")).unwrap();
        fast.index_root(&root, &IndexOptions::default()).unwrap();
        fast.publish_snapshot(&fast_source).unwrap();
        write_json_sync(
            &source.join("collection.json"),
            &Collection {
                format_version: 1,
                shards: vec![
                    ShardSource {
                        name: "fast".into(),
                        source: fast_source.clone(),
                        visibility: Visibility::default(),
                    },
                    ShardSource {
                        name: "base".into(),
                        source: base_source.clone(),
                        visibility: Visibility {
                            exclude_paths: vec![root.clone()],
                            exclude_kinds: vec!["agent_memory".into()],
                        },
                    },
                ],
            },
        )
        .unwrap();
        let cache = fixture.path().join("cache");
        let initial = refresh(&source, &cache, None).unwrap().unwrap();
        let mut reader = ReaderIndex::open(&initial.path).unwrap();
        let scoped = reader
            .search_filtered(
                "scopedneedle",
                5,
                std::slice::from_ref(&child),
                &[],
                None,
                None,
            )
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].path, child.join("chosen.txt").to_string_lossy());
        // Literal-path retrieval must not revive a deleted, non-authoritative copy.
        assert!(
            reader
                .search_filtered("old0.rs", 5, &[], &[], None, None,)
                .unwrap()
                .iter()
                .all(|hit| !hit.path.ends_with("/old0.rs"))
        );
        let hits = reader
            .search_filtered("federationneedle", 5, &[], &[], None, Some(&root))
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert!(
            hits.iter()
                .any(|hit| hit.path == data.join("kept.rs").to_string_lossy())
        );
        let query = QueryRequest {
            sql: "SELECT a.value + b.value FROM a CROSS JOIN b".into(),
            roots: vec![root.clone(), data.clone()],
            inputs: vec![
                QueryInput {
                    path: root.join("rows.csv"),
                    alias: Some("a".into()),
                },
                QueryInput {
                    path: data.join("rows.csv"),
                    alias: Some("b".into()),
                },
            ],
            max_rows: 10,
            max_bytes: 4096,
            timeout_ms: 5000,
        };
        assert_eq!(reader.query(&query).unwrap().rows[0][0], 12);
        let mut invalid = query.clone();
        invalid.roots = vec![fixture.path().to_owned()];
        assert!(reader.query(&invalid).is_err());

        fs::remove_file(&file).unwrap();
        fast.notify_paths(std::slice::from_ref(&file), &IndexOptions::default())
            .unwrap();
        fast.publish_snapshot(&fast_source).unwrap();
        // An unchanged base must be reused without re-reading its large files.
        let pointer = read_pointer(&base_source).unwrap();
        let old_manifest = base_source
            .join("generations")
            .join(pointer.directory)
            .join("manifest.json");
        let saved_manifest = fixture.path().join("saved-manifest");
        // Source snapshots are immutable; emulate temporary unavailability by
        // renaming their parent directory instead of modifying their contents.
        let generations = base_source.join("generations");
        fs::rename(&generations, &saved_manifest).unwrap();
        let updated = refresh(&source, &cache, Some(&initial)).unwrap().unwrap();
        fs::rename(&saved_manifest, &generations).unwrap();
        assert!(old_manifest.exists());
        reader = reader.open_reader_at(&updated.path).unwrap();
        let hits = reader
            .search_filtered("federationneedle", 5, &[], &[], None, Some(&root))
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, data.join("kept.rs").to_string_lossy());
        assert!(reader.inspect_excerpt(&file, 1, 10, 1000).is_err());
        assert!(refresh(&source, &cache, Some(&updated)).unwrap().is_none());
        let status = reader.status().unwrap();
        assert_eq!(status.shards.len(), 2);
        assert_eq!(status.completed_generation, None);

        fs::write(&file, "fn changedneedle() {}\n").unwrap();
        fast.notify_paths(std::slice::from_ref(&file), &IndexOptions::default())
            .unwrap();
        fast.publish_snapshot(&fast_source).unwrap();
        let mut pointer = read_pointer(&fast_source).unwrap();
        pointer.directory = "missing".into();
        write_json_sync(&fast_source.join("current.json"), &pointer).unwrap();
        assert!(refresh(&source, &cache, Some(&updated)).is_err());
        assert_eq!(
            reader
                .search_filtered("federationneedle", 5, &[], &[], None, None)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn cache_retains_inflight_reader_and_reclaims_it_after_release() {
        let fixture = tempdir().unwrap();
        let root = fixture.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let file = root.join("main.rs");
        let source = fixture.path().join("source");
        let cache = fixture.path().join("cache");
        let mut writer = WorkspaceIndex::open(fixture.path().join("writer")).unwrap();
        fs::write(&file, "fn first_version() {}").unwrap();
        writer.index_root(&root, &IndexOptions::default()).unwrap();
        writer.publish_snapshot(&source).unwrap();
        let first = refresh(&source, &cache, None).unwrap().unwrap();
        let held = ReaderIndex::open(&first.path).unwrap();
        let mut active = first.clone();
        for i in 0..5 {
            fs::write(&file, format!("fn version_{i}() {{}}")).unwrap();
            writer
                .notify_paths(std::slice::from_ref(&file), &IndexOptions::default())
                .unwrap();
            writer.publish_snapshot(&source).unwrap();
            active = refresh(&source, &cache, Some(&active)).unwrap().unwrap();
        }
        assert!(first.path.exists());
        drop(held);
        fs::write(&file, "fn final_version() {}").unwrap();
        writer
            .notify_paths(&[file], &IndexOptions::default())
            .unwrap();
        writer.publish_snapshot(&source).unwrap();
        refresh(&source, &cache, Some(&active)).unwrap().unwrap();
        assert!(!first.path.exists());
        assert_eq!(fs::read_dir(cache.join("generations")).unwrap().count(), 3);
    }

    #[test]
    fn same_generation_replacement_has_a_new_identity_and_cache_path() {
        let fixture = tempdir().unwrap();
        let root = fixture.path().join("workspace");
        fs::create_dir_all(&root).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "fn firstneedle() {}\n").unwrap();
        let source = fixture.path().join("source");
        let cache = fixture.path().join("cache");
        let mut first = WorkspaceIndex::open(fixture.path().join("first")).unwrap();
        first.index_root(&root, &IndexOptions::default()).unwrap();
        first.publish_snapshot(&source).unwrap();
        let active = refresh(&source, &cache, None).unwrap().unwrap();
        fs::write(&file, "fn secondneedle() {}\n").unwrap();
        let mut second = WorkspaceIndex::open(fixture.path().join("second")).unwrap();
        second.index_root(&root, &IndexOptions::default()).unwrap();
        let source2 = fixture.path().join("source2");
        second.publish_snapshot(&source2).unwrap();
        let mut pointer = read_pointer(&source2).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            source2.join("generations").join(&pointer.directory),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        fs::rename(
            source2.join("generations").join(&pointer.directory),
            source.join("generations/replacement"),
        )
        .unwrap();
        pointer.directory = "replacement".into();
        write_json_sync(&source.join("current.json"), &pointer).unwrap();
        let updated = refresh(&source, &cache, Some(&active)).unwrap().unwrap();
        assert_eq!(updated.generation, active.generation);
        assert_ne!(updated.identity, active.identity);
        assert_ne!(updated.path, active.path);
        let mut reader = ReaderIndex::open(&updated.path).unwrap();
        assert_eq!(
            reader
                .search_filtered("secondneedle", 5, &[], &[], None, None)
                .unwrap()
                .len(),
            1
        );
    }
}
