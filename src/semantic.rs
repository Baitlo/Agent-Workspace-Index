use std::collections::{BTreeSet, HashMap, HashSet};
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::model::{SearchCandidate, SearchDocument, SemanticCandidate, SemanticStatus};

const SIDECAR_SOURCE: &str = include_str!("../scripts/semantic_sidecar.py");
const DEFAULT_THREADS: usize = 16;
const DEFAULT_BATCH_SIZE: usize = 16;
const DEFAULT_EMBEDDING_WORKERS: usize = 1;
const DEFAULT_VECTOR_LIMIT: usize = 50;
const DEFAULT_VECTOR_WEIGHT: f32 = 1.0;
const DEFAULT_MAX_CHUNKS_PER_FILE: usize = 4;
const DEFAULT_QUERY_TIMEOUT_MS: u64 = 2_000;
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
            vector_limit: env_usize("AWI_SEMANTIC_VECTOR_LIMIT", DEFAULT_VECTOR_LIMIT)?,
            vector_weight: env_f32("AWI_SEMANTIC_WEIGHT", DEFAULT_VECTOR_WEIGHT)?,
            query_timeout: Duration::from_millis(env_u64(
                "AWI_SEMANTIC_QUERY_TIMEOUT_MS",
                DEFAULT_QUERY_TIMEOUT_MS,
            )?),
        }))
    }
}

pub(crate) struct SemanticIndex {
    index_dir: PathBuf,
    config: Option<SemanticConfig>,
    runtime: Mutex<Option<SemanticRuntime>>,
}

impl SemanticIndex {
    pub(crate) fn open(index_dir: &Path) -> Result<Self> {
        Ok(Self {
            index_dir: index_dir.to_owned(),
            config: SemanticConfig::from_env()?,
            runtime: Mutex::new(None),
        })
    }

    #[cfg(test)]
    pub(crate) fn disabled(index_dir: &Path) -> Self {
        Self {
            index_dir: index_dir.to_owned(),
            config: None,
            runtime: Mutex::new(None),
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
        self.search("warm semantic retrieval", generation, &[], &[], &[])?;
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
    ) -> Result<Vec<SemanticCandidate>> {
        let Some(config) = &self.config else {
            return Ok(Vec::new());
        };
        if !self.manifest().is_file() || !self.database().is_dir() {
            return Ok(Vec::new());
        }
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
        }))?;
        serde_json::from_value(result["candidates"].clone()).context("decode semantic candidates")
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
                ..SemanticStatus::default()
            };
        }
        match fs::read(self.manifest())
            .context("read semantic manifest")
            .and_then(|bytes| {
                serde_json::from_slice::<Value>(&bytes).context("decode semantic manifest")
            }) {
            Ok(manifest) => SemanticStatus {
                enabled,
                available: true,
                generation: manifest["generation"].as_i64(),
                files: manifest["files"].as_u64().unwrap_or_default(),
                chunks: manifest["rows"].as_u64().unwrap_or_default(),
                error: None,
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
        let mut guard = self
            .runtime
            .lock()
            .expect("semantic runtime mutex poisoned");
        if guard.is_none() {
            *guard = Some(SemanticRuntime::start(
                self.config
                    .as_ref()
                    .context("semantic runtime is not configured")?,
            )?);
        }
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
        let result = guard
            .as_mut()
            .expect("semantic runtime initialized")
            .request(&request, timeout);
        if result.is_err() {
            guard.take();
        }
        result
    }
}

struct SemanticRuntime {
    child: Child,
    socket: PathBuf,
}

impl SemanticRuntime {
    fn start(config: &SemanticConfig) -> Result<Self> {
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
            .arg("--socket")
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        if let Some(checksum) = &config.model_sha256 {
            command.arg("--model-sha256").arg(checksum);
        }
        let child = command.spawn().with_context(|| {
            format!(
                "start semantic sidecar with Python {}",
                config.python.display()
            )
        })?;
        let mut runtime = Self { child, socket };
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            if let Some(status) = runtime.child.try_wait()? {
                anyhow::bail!("semantic sidecar exited during startup: {status}");
            }
            if runtime.socket.exists() {
                let ready = runtime
                    .request(&json!({"op": "ping"}), config.query_timeout)
                    .context("ping semantic sidecar during startup")?;
                if ready["ready"].as_bool().unwrap_or(false) {
                    return Ok(runtime);
                }
            }
            if Instant::now() >= deadline {
                anyhow::bail!("semantic sidecar did not start within {START_TIMEOUT:?}");
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn request(&mut self, request: &Value, timeout: Duration) -> Result<Value> {
        let mut stream = UnixStream::connect(&self.socket)
            .with_context(|| format!("connect semantic sidecar {}", self.socket.display()))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        serde_json::to_writer(&mut stream, request)?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        read_response(&mut BufReader::new(stream))
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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
            });
        entry.score += contribution;
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
        assert!(index.search("query", 1, &[], &[], &[]).unwrap().is_empty());
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
}
