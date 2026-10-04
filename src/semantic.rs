use std::collections::{BTreeSet, HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::metrics::LatencyWindow;
use crate::model::{
    SearchCandidate, SearchDocument, SemanticCandidate, SemanticMetrics, SemanticStatus,
};

const SIDECAR_SOURCE: &str = include_str!("../scripts/semantic_sidecar.py");
const DEFAULT_THREADS: usize = 16;
const DEFAULT_BATCH_SIZE: usize = 16;
const DEFAULT_EMBEDDING_WORKERS: usize = 1;
const DEFAULT_VECTOR_LIMIT: usize = 50;
const DEFAULT_VECTOR_WEIGHT: f32 = 1.0;
const DEFAULT_MAX_CHUNKS_PER_FILE: usize = 4;
const DEFAULT_MAX_SQL_CHUNKS_PER_FILE: usize = 8;
const DEFAULT_QUERY_BATCH_SIZE: usize = 8;
const DEFAULT_QUERY_BATCH_WAIT_MS: u64 = 2;
const DEFAULT_QUERY_CACHE_SIZE: usize = 256;
const DEFAULT_MAX_INFLIGHT: usize = 32;
const DEFAULT_QUERY_TIMEOUT_MS: u64 = 2_000;
const CHUNKING_VERSION: u64 = 2;
const UPDATE_TIMEOUT: Duration = Duration::from_secs(600);
const START_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_BATCH_DOCUMENTS: usize = 32;
const MAX_BATCH_BYTES: usize = 8 * 1024 * 1024;
const MAX_SEMANTIC_CONTENT_BYTES: usize = 64 * 1024;
const RRF_K: f32 = 60.0;
static NEXT_SOCKET_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct SemanticConfig {
    pub model: PathBuf,
    pub model_sha256: Option<String>,
    pub python: PathBuf,
    pub threads: usize,
    pub batch_size: usize,
    pub embedding_workers: usize,
    pub max_chunks_per_file: usize,
    pub max_sql_chunks_per_file: usize,
    pub query_batch_size: usize,
    pub query_batch_wait_ms: u64,
    pub query_cache_size: usize,
    pub max_inflight: usize,
    pub vector_limit: usize,
    pub vector_weight: f32,
    pub query_timeout: Duration,
}

impl SemanticConfig {
    pub fn from_env() -> Result<Option<Self>> {
        let Some(model) = env::var_os("AWI_SEMANTIC_MODEL") else {
            return Ok(None);
        };
        let model = PathBuf::from(model);
        if !model.is_file() {
            anyhow::bail!("AWI semantic model does not exist: {}", model.display());
        }
        Ok(Some(Self {
            model,
            model_sha256: env::var("AWI_SEMANTIC_MODEL_SHA256").ok(),
            python: env::var_os("AWI_SEMANTIC_PYTHON")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("python3")),
            threads: env_usize("AWI_SEMANTIC_THREADS", DEFAULT_THREADS)?,
            batch_size: env_usize("AWI_SEMANTIC_BATCH_SIZE", DEFAULT_BATCH_SIZE)?,
            embedding_workers: env_usize("AWI_SEMANTIC_EMBED_WORKERS", DEFAULT_EMBEDDING_WORKERS)?,
            max_chunks_per_file: env_usize(
                "AWI_SEMANTIC_MAX_CHUNKS_PER_FILE",
                DEFAULT_MAX_CHUNKS_PER_FILE,
            )?,
            max_sql_chunks_per_file: env_usize(
                "AWI_SEMANTIC_MAX_SQL_CHUNKS_PER_FILE",
                DEFAULT_MAX_SQL_CHUNKS_PER_FILE,
            )?,
            query_batch_size: env_usize("AWI_SEMANTIC_QUERY_BATCH_SIZE", DEFAULT_QUERY_BATCH_SIZE)?,
            query_batch_wait_ms: env_u64(
                "AWI_SEMANTIC_QUERY_BATCH_WAIT_MS",
                DEFAULT_QUERY_BATCH_WAIT_MS,
            )?,
            query_cache_size: env_usize("AWI_SEMANTIC_QUERY_CACHE_SIZE", DEFAULT_QUERY_CACHE_SIZE)?,
            max_inflight: env_usize("AWI_SEMANTIC_MAX_INFLIGHT", DEFAULT_MAX_INFLIGHT)?,
            vector_limit: env_usize("AWI_SEMANTIC_VECTOR_LIMIT", DEFAULT_VECTOR_LIMIT)?,
            vector_weight: env_f32("AWI_SEMANTIC_WEIGHT", DEFAULT_VECTOR_WEIGHT)?,
            query_timeout: Duration::from_millis(env_u64(
                "AWI_SEMANTIC_QUERY_TIMEOUT_MS",
                DEFAULT_QUERY_TIMEOUT_MS,
            )?),
        }))
    }
}

/// Normalize a sidecar stderr line for dedupe: collapse digit runs to `#` so
/// lines like `partition 3 is empty, skipping` and `partition 7 is empty,
/// skipping` share one dedupe key while distinct messages stay distinct.
fn normalize_sidecar_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut last_was_digit = false;
    for ch in line.chars() {
        if ch.is_ascii_digit() {
            if !last_was_digit {
                out.push('#');
            }
            last_was_digit = true;
        } else {
            out.push(ch);
            last_was_digit = false;
        }
    }
    out
}

pub(crate) struct SemanticIndex {
    index_dir: PathBuf,
    config: Option<SemanticConfig>,
    runtime: Arc<Mutex<Option<Arc<SemanticRuntime>>>>,
    /// Lock-free mirror of whether `runtime` currently holds a live sidecar.
    /// `status()` reads this instead of taking `runtime`'s mutex, which query
    /// workers hold for the whole (up to 60s) sidecar start; taking that mutex on
    /// the status path is what wedged the daemon. Updated by `request()` as the
    /// runtime starts/stops.
    runtime_ready: Arc<AtomicBool>,
    startup: Arc<StartupGuard>,
    metrics: Arc<SemanticMetricsState>,
}

impl SemanticIndex {
    pub(crate) fn open(index_dir: &Path) -> Result<Self> {
        Ok(Self {
            index_dir: index_dir.to_owned(),
            config: SemanticConfig::from_env()?,
            runtime: Arc::new(Mutex::new(None)),
            runtime_ready: Arc::new(AtomicBool::new(false)),
            startup: Arc::new(StartupGuard::default()),
            metrics: Arc::new(SemanticMetricsState::default()),
        })
    }

    #[cfg(test)]
    pub(crate) fn disabled(index_dir: &Path) -> Self {
        Self {
            index_dir: index_dir.to_owned(),
            config: None,
            runtime: Arc::new(Mutex::new(None)),
            runtime_ready: Arc::new(AtomicBool::new(false)),
            startup: Arc::new(StartupGuard::default()),
            metrics: Arc::new(SemanticMetricsState::default()),
        }
    }

    pub(crate) fn retarget(&self, index_dir: &Path) -> Self {
        Self {
            index_dir: index_dir.to_owned(),
            config: self.config.clone(),
            runtime: Arc::clone(&self.runtime),
            runtime_ready: Arc::clone(&self.runtime_ready),
            startup: Arc::clone(&self.startup),
            metrics: Arc::clone(&self.metrics),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.config.is_some()
    }

    pub(crate) fn warm(&self) -> Result<()> {
        if self.config.is_none() {
            return Ok(());
        }
        if !self.manifest().is_file() || !self.database().is_dir() {
            self.request(json!({"op": "warm"}))?;
            return Ok(());
        }
        let manifest: Value = serde_json::from_slice(
            &fs::read(self.manifest()).context("read semantic manifest for warmup")?,
        )
        .context("decode semantic manifest for warmup")?;
        let generation = manifest["generation"]
            .as_i64()
            .context("semantic manifest has no generation")?;
        self.search("warm semantic retrieval", generation, &[], &[], &[])
            .map(|_candidates| ())?;
        Ok(())
    }

    pub(crate) fn reset(&self) -> Result<()> {
        if self.config.is_none() {
            anyhow::bail!("semantic indexing requires AWI_SEMANTIC_MODEL");
        }
        self.request(json!({
            "op": "reset",
            "database": self.database()
        }))?;
        let manifest = self.manifest();
        if manifest.exists() {
            fs::remove_file(&manifest)
                .with_context(|| format!("remove semantic manifest {}", manifest.display()))?;
        }
        Ok(())
    }

    pub(crate) fn file_generations(&self) -> Result<HashSet<(i64, i64)>> {
        if self.config.is_none() {
            anyhow::bail!("semantic indexing requires AWI_SEMANTIC_MODEL");
        }
        if !self.database().is_dir() {
            return Ok(HashSet::new());
        }
        let result = self.request(json!({
            "op": "coverage",
            "database": self.database()
        }))?;
        let pairs: Vec<(i64, i64)> =
            serde_json::from_value(result["pairs"].clone()).context("decode semantic coverage")?;
        Ok(pairs.into_iter().collect())
    }

    pub(crate) fn sql_chunks_are_current(&self) -> Result<bool> {
        if !self.manifest().is_file() {
            return Ok(false);
        }
        let manifest: Value =
            serde_json::from_slice(&fs::read(self.manifest()).context("read semantic manifest")?)
                .context("decode semantic manifest")?;
        Ok(manifest["chunking_version"].as_u64() == Some(CHUNKING_VERSION))
    }

    pub(crate) fn apply_changes(
        &self,
        documents: &[SearchDocument],
        deleted_paths: &[String],
    ) -> Result<(u64, u64)> {
        if self.config.is_none() {
            return Ok((0, 0));
        }
        let replaced_paths = replacement_paths(documents, deleted_paths);
        let documents = documents
            .iter()
            .filter(|document| semantic_kind(&document.kind))
            .collect::<Vec<_>>();
        let mut files = 0_u64;
        let mut chunks = 0_u64;
        for (index, batch) in document_batches(&documents).into_iter().enumerate() {
            let removed: &[String] = if index == 0 {
                replaced_paths.as_slice()
            } else {
                &[]
            };
            let documents = batch
                .iter()
                .map(|document| {
                    json!({
                        "file_id": document.file_id,
                        "path": document.path,
                        "kind": document.kind,
                        "generation": document.generation,
                        "content": semantic_text(document)
                    })
                })
                .collect::<Vec<_>>();
            let result = self.request(json!({
                "op": "update",
                "database": self.database(),
                "documents": documents,
                "deleted_paths": removed
            }))?;
            files += result["files"].as_u64().unwrap_or_default();
            chunks += result["chunks"].as_u64().unwrap_or_default();
        }
        if documents.is_empty() && !replaced_paths.is_empty() {
            self.request(json!({
                "op": "update",
                "database": self.database(),
                "documents": [],
                "deleted_paths": replaced_paths
            }))?;
        }
        Ok((files, chunks))
    }

    pub(crate) fn seal(&self, generation: i64, expected: &[(i64, i64)]) -> Result<()> {
        if self.config.is_none() {
            return Ok(());
        }
        self.request(json!({
            "op": "seal",
            "database": self.database(),
            "manifest": self.manifest(),
            "generation": generation,
            "expected": expected
        }))?;
        Ok(())
    }

    pub(crate) fn search(
        &self,
        query: &str,
        generation: i64,
        roots: &[PathBuf],
        kinds: &[String],
        path_prefixes: &[String],
    ) -> Result<(Vec<SemanticCandidate>, Value)> {
        let Some(config) = &self.config else {
            return Ok((Vec::new(), Value::Null));
        };
        if !self.manifest().is_file() || !self.database().is_dir() {
            return Ok((Vec::new(), Value::Null));
        }
        let started_at = Instant::now();
        let result = self.request(json!({
            "op": "query",
            "database": self.database(),
            "manifest": self.manifest(),
            "generation": generation,
            "query": query,
            "limit": config.vector_limit,
            "roots": roots,
            "kinds": kinds,
            "path_prefixes": path_prefixes
        }));
        match result {
            Ok(result) => {
                // The diagnostics dict is the per-query stage contract handed to
                // the MCP/audit layer: the caller (workspace search_filtered)
                // owns whether/how it reaches the audit record; this crate only
                // guarantees a real, non-empty diagnostics Value per query.
                let diagnostics = result["diagnostics"].clone();
                self.metrics.record(&diagnostics, started_at.elapsed());
                let candidates: Vec<SemanticCandidate> =
                    serde_json::from_value(result["candidates"].clone())
                        .context("decode semantic candidates")?;
                Ok((candidates, diagnostics))
            }
            Err(error) => {
                self.metrics.record_failure(started_at.elapsed());
                Err(error)
            }
        }
    }

    pub(crate) fn vector_weight(&self) -> f32 {
        self.config
            .as_ref()
            .map_or(DEFAULT_VECTOR_WEIGHT, |config| config.vector_weight)
    }

    pub(crate) fn status(&self) -> SemanticStatus {
        let enabled = self.config.is_some();
        if !self.manifest().is_file() || !self.database().is_dir() {
            return SemanticStatus {
                enabled,
                effective_mode: "lexical_only".to_owned(),
                ..SemanticStatus::default()
            };
        }
        // Lock-free observation of sidecar readiness. We deliberately do NOT take
        // `self.runtime` here: query workers hold that mutex for the entire (up to
        // 60s) sidecar start, and a blocked status call is what wedged the accept
        // loop. `runtime_ready` is maintained by `request()`. Only bounded manifest
        // reads happen here; no sidecar start, no synchronous query.
        let ready = self.runtime_ready.load(Ordering::Relaxed);
        match fs::read(self.manifest())
            .context("read semantic manifest")
            .and_then(|bytes| {
                serde_json::from_slice::<Value>(&bytes).context("decode semantic manifest")
            }) {
            Ok(manifest) => SemanticStatus {
                enabled,
                available: true,
                runtime_ready: ready,
                startup_failures: self.startup.failures() as u64,
                effective_mode: if ready {
                    "semantic".to_owned()
                } else {
                    "lexical_only".to_owned()
                },
                fallback_reason: if ready {
                    None
                } else {
                    self.startup.fallback_reason()
                },
                generation: manifest["generation"].as_i64(),
                files: manifest["files"].as_u64().unwrap_or_default(),
                chunks: manifest["rows"].as_u64().unwrap_or_default(),
                chunking_version: manifest["chunking_version"].as_u64().unwrap_or_default(),
                metrics: self.metrics.snapshot(),
                error: None,
                ..SemanticStatus::default()
            },
            Err(error) => SemanticStatus {
                enabled,
                available: false,
                error: Some(format!("{error:#}")),
                ..SemanticStatus::default()
            },
        }
    }

    fn database(&self) -> PathBuf {
        self.index_dir.join("semantic.lance")
    }

    fn manifest(&self) -> PathBuf {
        self.index_dir.join("semantic-manifest.json")
    }

    fn request(&self, request: Value) -> Result<Value> {
        let runtime = {
            let mut guard = self
                .runtime
                .lock()
                .expect("semantic runtime mutex poisoned");
            if guard.is_none() {
                // Circuit breaker: a deterministic startup failure does not heal
                // itself, so do not burn a 60s spawn budget on every query.
                if let Some(until) = self.startup.open_until()
                    && Instant::now() < until
                {
                    let reason = self
                        .startup
                        .fallback_reason()
                        .unwrap_or_else(|| "semantic sidecar in backoff".to_owned());
                    anyhow::bail!("semantic circuit open ({})", reason);
                }
                let started = Instant::now();
                match SemanticRuntime::start(
                    self.config
                        .as_ref()
                        .context("semantic runtime is not configured")?,
                ) {
                    Ok(runtime) => {
                        self.startup.reset();
                        self.metrics
                            .sidecar_startup
                            .record_ms(started.elapsed().as_secs_f64() * 1000.0);
                        self.metrics.sidecar_starts.fetch_add(1, Ordering::Relaxed);
                        *guard = Some(Arc::new(runtime));
                        self.runtime_ready.store(true, Ordering::Relaxed);
                    }
                    Err(error) => {
                        self.startup.open_cooldown(&error);
                        self.metrics
                            .startup_failures
                            .fetch_add(1, Ordering::Relaxed);
                        return Err(error);
                    }
                }
            }
            Arc::clone(guard.as_ref().expect("semantic runtime initialized"))
        };
        let timeout = if matches!(
            request.get("op").and_then(Value::as_str),
            Some("query" | "ping")
        ) {
            self.config
                .as_ref()
                .context("semantic runtime is not configured")?
                .query_timeout
        } else {
            UPDATE_TIMEOUT
        };
        let result = runtime.request(&request, timeout);
        if result.is_err() {
            let mut guard = self
                .runtime
                .lock()
                .expect("semantic runtime mutex poisoned");
            if guard
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, &runtime))
            {
                guard.take();
                self.runtime_ready.store(false, Ordering::Relaxed);
                runtime.stop();
            }
        }
        result
    }
}

#[derive(Default)]
struct SemanticMetricsState {
    queries: AtomicU64,
    result_cache_hits: AtomicU64,
    embedding_cache_hits: AtomicU64,
    coalesced_queries: AtomicU64,
    failures: AtomicU64,
    sidecar_starts: AtomicU64,
    startup_failures: AtomicU64,
    queue_wait: LatencyWindow,
    sidecar_startup: LatencyWindow,
    embedding: LatencyWindow,
    snapshot_open: LatencyWindow,
    serialization: LatencyWindow,
    vector_search: LatencyWindow,
    total: LatencyWindow,
    /// Number of queries that actually performed a LanceDB connect + open_table
    /// (as opposed to reusing a cached handle). The snapshot_open window only
    /// contains these real open events.
    snapshot_opened: AtomicU64,
}

impl SemanticMetricsState {
    fn record(&self, diagnostics: &Value, elapsed: Duration) {
        self.queries.fetch_add(1, Ordering::Relaxed);
        if diagnostics["result_cache_hit"].as_bool().unwrap_or(false) {
            self.result_cache_hits.fetch_add(1, Ordering::Relaxed);
        }
        if diagnostics["embedding_cache_hit"]
            .as_bool()
            .unwrap_or(false)
        {
            self.embedding_cache_hits.fetch_add(1, Ordering::Relaxed);
        }
        if diagnostics["coalesced"].as_bool().unwrap_or(false) {
            self.coalesced_queries.fetch_add(1, Ordering::Relaxed);
        }
        // Sidecar pipeline queue wait: socket accept -> worker start.
        self.queue_wait
            .record_ms(diagnostics["queue_wait_ms"].as_f64().unwrap_or_default());
        self.embedding
            .record_ms(diagnostics["embedding_ms"].as_f64().unwrap_or_default());
        // The semantic snapshot-open window records ONLY real
        // connect + open_table events. A cached-handle query carries
        // snapshot_opened=false and a null semantic_snapshot_open_ms; it must
        // not push a zero sample or re-report the previous open cost.
        if diagnostics["snapshot_opened"].as_bool().unwrap_or(false) {
            self.snapshot_open.record_ms(
                diagnostics["semantic_snapshot_open_ms"]
                    .as_f64()
                    .unwrap_or_default(),
            );
            self.snapshot_opened.fetch_add(1, Ordering::Relaxed);
        }
        self.serialization
            .record_ms(diagnostics["serialization_ms"].as_f64().unwrap_or_default());
        self.vector_search
            .record_ms(diagnostics["vector_ms"].as_f64().unwrap_or_default());
        self.total.record_ms(
            diagnostics["total_ms"]
                .as_f64()
                .unwrap_or(elapsed.as_secs_f64() * 1_000.0),
        );
    }

    fn record_failure(&self, elapsed: Duration) {
        self.queries.fetch_add(1, Ordering::Relaxed);
        self.failures.fetch_add(1, Ordering::Relaxed);
        self.total.record(elapsed);
    }

    fn snapshot(&self) -> SemanticMetrics {
        let starts = self.sidecar_starts.load(Ordering::Relaxed);
        SemanticMetrics {
            queries: self.queries.load(Ordering::Relaxed),
            result_cache_hits: self.result_cache_hits.load(Ordering::Relaxed),
            embedding_cache_hits: self.embedding_cache_hits.load(Ordering::Relaxed),
            coalesced_queries: self.coalesced_queries.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
            sidecar_starts: starts,
            sidecar_restarts: starts.saturating_sub(1),
            startup_failures: self.startup_failures.load(Ordering::Relaxed),
            queue_wait: self.queue_wait.snapshot(),
            sidecar_startup: self.sidecar_startup.snapshot(),
            embedding: self.embedding.snapshot(),
            snapshot_open: self.snapshot_open.snapshot(),
            serialization: self.serialization.snapshot(),
            vector_search: self.vector_search.snapshot(),
            total: self.total.snapshot(),
            snapshot_opened: self.snapshot_opened.load(Ordering::Relaxed),
        }
    }
}

/// Exponential-backoff circuit breaker for sidecar startup.
///
/// A deterministic import/config failure makes every respawn fail the same way;
/// without state the reader would retry the (60s-budget) spawn on every single
/// query. This guard opens for a backoff window and returns an immediate lexical
/// fallback instead.
#[derive(Default)]
pub(crate) struct StartupGuard {
    cooldown_until: Mutex<Option<Instant>>,
    failures: Mutex<u32>,
    last_error: Mutex<Option<String>>,
    last_ready: Mutex<Option<Instant>>,
}

impl StartupGuard {
    fn open_cooldown(&self, error: &anyhow::Error) {
        let mut failures = self.failures.lock().expect("startup failures poisoned");
        *failures = failures.saturating_add(1);
        let exp = (*failures).min(8);
        let backoff = Duration::from_secs(2u64.saturating_mul(1u64 << exp));
        *self.cooldown_until.lock().expect("cooldown poisoned") =
            Some(Instant::now() + backoff.min(Duration::from_secs(120)));
        *self.last_error.lock().expect("last error poisoned") = Some(format!("{error:#}"));
    }

    fn reset(&self) {
        *self.failures.lock().expect("failures poisoned") = 0;
        *self.cooldown_until.lock().expect("cooldown poisoned") = None;
        *self.last_ready.lock().expect("last ready poisoned") = Some(Instant::now());
    }

    fn open_until(&self) -> Option<Instant> {
        *self.cooldown_until.lock().expect("cooldown poisoned")
    }

    fn failures(&self) -> u32 {
        *self.failures.lock().expect("failures poisoned")
    }

    fn fallback_reason(&self) -> Option<String> {
        self.last_error.lock().expect("last error poisoned").clone()
    }
}

struct SemanticRuntime {
    child: Mutex<Child>,
    socket: PathBuf,
}

impl SemanticRuntime {
    fn start(config: &SemanticConfig) -> Result<Self> {
        let started = Instant::now();
        let script = materialize_sidecar()?;
        let socket = env::temp_dir().join(format!(
            "awi-semantic-{}-{}.sock",
            std::process::id(),
            NEXT_SOCKET_ID.fetch_add(1, Ordering::Relaxed)
        ));
        if socket.exists() {
            fs::remove_file(&socket)
                .with_context(|| format!("remove stale semantic socket {}", socket.display()))?;
        }
        let mut command = Command::new(&config.python);
        command
            .arg("-u")
            .arg(script)
            .arg("--model")
            .arg(&config.model)
            .arg("--threads")
            .arg(config.threads.to_string())
            .arg("--batch-size")
            .arg(config.batch_size.to_string())
            .arg("--embedding-workers")
            .arg(config.embedding_workers.to_string())
            .arg("--max-chunks-per-file")
            .arg(config.max_chunks_per_file.to_string())
            .arg("--max-sql-chunks-per-file")
            .arg(config.max_sql_chunks_per_file.to_string())
            .arg("--query-batch-size")
            .arg(config.query_batch_size.to_string())
            .arg("--query-batch-wait-ms")
            .arg(config.query_batch_wait_ms.to_string())
            .arg("--query-cache-size")
            .arg(config.query_cache_size.to_string())
            .arg("--max-inflight")
            .arg(config.max_inflight.to_string())
            .arg("--socket")
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if let Some(checksum) = &config.model_sha256 {
            command.arg("--model-sha256").arg(checksum);
        }
        let mut child = command.spawn().with_context(|| {
            format!(
                "start semantic sidecar with Python {}",
                config.python.display()
            )
        })?;
        // Empty-partition KMeans/cluster warnings from the LanceDB Rust core
        // repeat on every IVF build (thousands of lines in producer logs).
        // lancedb 0.39 does not honor RUST_LOG for the compiled tracing
        // layer, so dedupe sidecar stderr here: the first occurrence of each
        // normalized line still reaches the daemon log as a valid diagnostic,
        // repeats are counted and summarized once when the sidecar exits.
        if let Some(stderr) = child.stderr.take() {
            std::thread::Builder::new()
                .name("awi-semantic-stderr".into())
                .spawn(move || {
                    use std::io::BufRead;
                    let reader = std::io::BufReader::new(stderr);
                    let mut seen: std::collections::HashMap<String, u64> =
                        std::collections::HashMap::new();
                    for line in reader.lines().map_while(Result::ok) {
                        let key = normalize_sidecar_line(&line);
                        let entry = seen.entry(key.clone()).or_insert(0);
                        if *entry == 0 {
                            eprintln!("{line}");
                        }
                        *entry += 1;
                    }
                    for (line, count) in seen {
                        if count > 1 {
                            eprintln!("awi-semantic: suppressed {count} repeats of: {line}");
                        }
                    }
                })
                .ok();
        }
        let runtime = Self {
            child: Mutex::new(child),
            socket,
        };
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if let Some(status) = runtime
                .child
                .lock()
                .expect("semantic child mutex poisoned")
                .try_wait()?
            {
                anyhow::bail!("semantic sidecar exited during startup: {status}");
            }
            if runtime.socket.exists() {
                let ready = runtime
                    .request(&json!({"op": "ping"}), config.query_timeout)
                    .context("ping semantic sidecar during startup")?;
                if ready["ready"].as_bool().unwrap_or(false) {
                    eprintln!(
                        "awi-semantic: sidecar ready pid={} startup_ms={:.2}",
                        runtime
                            .child
                            .lock()
                            .expect("semantic child mutex poisoned")
                            .id(),
                        started.elapsed().as_secs_f64() * 1_000.0
                    );
                    return Ok(runtime);
                }
            }
            if Instant::now() >= deadline {
                anyhow::bail!("semantic sidecar did not start within {START_TIMEOUT:?}");
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn request(&self, request: &Value, timeout: Duration) -> Result<Value> {
        let mut stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("connect semantic sidecar {}", self.socket.display()))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        serde_json::to_writer(&mut stream, request)?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        read_response(&mut BufReader::new(stream))
    }

    fn stop(&self) {
        let mut child = self.child.lock().expect("semantic child mutex poisoned");
        let _ = child.kill();
        let _ = child.wait();
        let _ = fs::remove_file(&self.socket);
    }
}

impl Drop for SemanticRuntime {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Deserialize)]
struct SidecarResponse {
    ok: bool,
    result: Option<Value>,
    error: Option<String>,
}

fn read_response(output: &mut impl BufRead) -> Result<Value> {
    let mut line = String::new();
    let read = output
        .read_line(&mut line)
        .context("read semantic sidecar response")?;
    if read == 0 {
        anyhow::bail!("semantic sidecar closed stdout");
    }
    let response: SidecarResponse =
        serde_json::from_str(&line).context("decode semantic sidecar response")?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        anyhow::bail!(
            "semantic sidecar error: {}",
            response.error.unwrap_or_else(|| "unknown error".to_owned())
        )
    }
}

fn materialize_sidecar() -> Result<PathBuf> {
    let digest = blake3::hash(SIDECAR_SOURCE.as_bytes()).to_hex();
    let path = env::temp_dir().join(format!("awi-semantic-sidecar-{digest}.py"));
    if path.exists() {
        let existing = fs::read_to_string(&path)
            .with_context(|| format!("read semantic sidecar {}", path.display()))?;
        if existing != SIDECAR_SOURCE {
            anyhow::bail!("semantic sidecar hash collision at {}", path.display());
        }
        return Ok(path);
    }
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, SIDECAR_SOURCE)
        .with_context(|| format!("write semantic sidecar {}", temporary.display()))?;
    match fs::rename(&temporary, &path) {
        Ok(()) => Ok(path),
        Err(_error) if path.exists() => {
            let _ = fs::remove_file(temporary);
            Ok(path)
        }
        Err(error) => {
            Err(error).with_context(|| format!("publish semantic sidecar {}", path.display()))
        }
    }
}

fn document_batches<'a>(documents: &'a [&'a SearchDocument]) -> Vec<Vec<&'a SearchDocument>> {
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut bytes = 0_usize;
    for document in documents {
        let document_bytes = document
            .content
            .len()
            .saturating_add(document.symbols.len())
            .saturating_add(document.schema.len())
            .min(MAX_SEMANTIC_CONTENT_BYTES)
            + document.path.len()
            + 256;
        if !current.is_empty()
            && (current.len() >= MAX_BATCH_DOCUMENTS
                || bytes.saturating_add(document_bytes) > MAX_BATCH_BYTES)
        {
            batches.push(std::mem::take(&mut current));
            bytes = 0;
        }
        current.push(*document);
        bytes = bytes.saturating_add(document_bytes);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

fn semantic_kind(kind: &str) -> bool {
    matches!(kind, "source" | "text" | "semi_structured" | "tabular")
}

fn replacement_paths(documents: &[SearchDocument], deleted_paths: &[String]) -> Vec<String> {
    deleted_paths
        .iter()
        .chain(documents.iter().map(|document| &document.path))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn bounded_content(content: &str) -> &str {
    if content.len() <= MAX_SEMANTIC_CONTENT_BYTES {
        return content;
    }
    let mut end = MAX_SEMANTIC_CONTENT_BYTES;
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    &content[..end]
}

fn semantic_text(document: &SearchDocument) -> String {
    let mut text = document.content.clone();
    if !document.symbols.is_empty() {
        text.push_str("\nSymbols: ");
        text.push_str(&document.symbols);
    }
    if !document.schema.is_empty() {
        text.push_str("\nSchema: ");
        text.push_str(&document.schema);
    }
    bounded_content(&text).to_owned()
}

pub(crate) fn merge_semantic_candidates(
    lexical: Vec<SearchCandidate>,
    semantic: Vec<SemanticCandidate>,
    limit: usize,
    vector_weight: f32,
) -> Vec<SearchCandidate> {
    let mut fused = HashMap::<i64, SearchCandidate>::new();
    for candidate in lexical {
        fused.insert(candidate.file_id, candidate);
    }
    for (offset, candidate) in semantic.into_iter().enumerate() {
        let contribution = vector_weight / (RRF_K + offset as f32 + 1.0);
        let entry = fused
            .entry(candidate.file_id)
            .or_insert_with(|| SearchCandidate {
                file_id: candidate.file_id,
                path: candidate.path,
                generation: candidate.generation,
                score: 0.0,
                lanes: Vec::new(),
                evidence: Vec::new(),
            });
        entry.score += contribution;
        entry.evidence.push(crate::model::LaneEvidence {
            name: "semantic".to_owned(),
            score: candidate.score,
            weight: vector_weight,
            contribution,
        });
        entry.lanes.push("semantic".to_owned());
        entry.lanes = entry
            .lanes
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    let mut output = fused.into_values().collect::<Vec<_>>();
    output.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
    });
    output.truncate(limit);
    output
}

fn env_usize(name: &str, default: usize) -> Result<usize> {
    let Some(value) = env::var_os(name) else {
        return Ok(default);
    };
    let parsed = value
        .to_string_lossy()
        .parse::<usize>()
        .with_context(|| format!("{name} must be a positive integer"))?;
    if parsed == 0 {
        anyhow::bail!("{name} must be positive");
    }
    Ok(parsed)
}

fn env_f32(name: &str, default: f32) -> Result<f32> {
    let Some(value) = env::var_os(name) else {
        return Ok(default);
    };
    let parsed = value
        .to_string_lossy()
        .parse::<f32>()
        .with_context(|| format!("{name} must be a positive number"))?;
    if !parsed.is_finite() || parsed <= 0.0 {
        anyhow::bail!("{name} must be positive and finite");
    }
    Ok(parsed)
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    let Some(value) = env::var_os(name) else {
        return Ok(default);
    };
    let parsed = value
        .to_string_lossy()
        .parse::<u64>()
        .with_context(|| format!("{name} must be a positive integer"))?;
    if parsed == 0 {
        anyhow::bail!("{name} must be positive");
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(file_id: i64, path: &str, kind: &str, content: &str) -> SearchDocument {
        SearchDocument {
            file_id,
            path: path.to_owned(),
            name: Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            experiment: String::new(),
            kind: kind.to_owned(),
            content: content.to_owned(),
            symbols: String::new(),
            schema: String::new(),
            preview: String::new(),
            generation: 3,
        }
    }

    fn lexical(file_id: i64, score: f32) -> SearchCandidate {
        SearchCandidate {
            file_id,
            path: format!("/workspace/{file_id}.rs"),
            generation: 3,
            score,
            lanes: vec!["content".to_owned()],
            evidence: Vec::new(),
        }
    }

    #[test]
    fn semantic_candidates_are_fused_and_deduplicated() {
        let output = merge_semantic_candidates(
            vec![lexical(1, 0.02), lexical(2, 0.01)],
            vec![
                SemanticCandidate {
                    file_id: 2,
                    path: "/workspace/2.rs".to_owned(),
                    generation: 3,
                    score: 0.9,
                },
                SemanticCandidate {
                    file_id: 3,
                    path: "/workspace/3.rs".to_owned(),
                    generation: 3,
                    score: 0.8,
                },
            ],
            10,
            1.0,
        );
        assert_eq!(output.len(), 3);
        assert_eq!(output[0].file_id, 2);
        assert!(output[0].lanes.contains(&"semantic".to_owned()));
        assert_eq!(
            output
                .iter()
                .find(|candidate| candidate.file_id == 3)
                .unwrap()
                .lanes,
            vec!["semantic"]
        );
    }

    #[test]
    fn disabled_index_has_no_runtime() {
        let fixture = tempfile::tempdir().unwrap();
        let index = SemanticIndex::disabled(fixture.path());
        assert!(!index.enabled());
        assert!(
            index
                .search("query", 1, &[], &[], &[])
                .unwrap()
                .0
                .is_empty()
        );
    }

    #[test]
    fn replacement_paths_keep_documents_without_semantic_payload() {
        let documents = vec![
            document(1, "/workspace/empty.rs", "source", ""),
            document(2, "/workspace/image.png", "binary", ""),
        ];
        assert_eq!(
            replacement_paths(&documents, &["/workspace/deleted.rs".to_owned()]),
            vec![
                "/workspace/deleted.rs",
                "/workspace/empty.rs",
                "/workspace/image.png"
            ]
        );
    }

    #[test]
    fn sidecar_stderr_dedupe_normalizes_partition_numbers() {
        assert_eq!(
            normalize_sidecar_line("partition 3 is empty, skipping"),
            normalize_sidecar_line("partition 7 is empty, skipping")
        );
        assert_ne!(
            normalize_sidecar_line("partition 3 is empty, skipping"),
            normalize_sidecar_line("index build failed")
        );
        assert_eq!(
            normalize_sidecar_line("12 of 20 clusters are empty: 12"),
            normalize_sidecar_line("11 of 20 clusters are empty: 11")
        );
    }
}
