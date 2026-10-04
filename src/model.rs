use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Source,
    Text,
    SemiStructured,
    Tabular,
    AgentInstructions,
    AgentSkill,
    AgentMemory,
    Binary,
    Unknown,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Text => "text",
            Self::SemiStructured => "semi_structured",
            Self::Tabular => "tabular",
            Self::AgentInstructions => "agent_instructions",
            Self::AgentSkill => "agent_skill",
            Self::AgentMemory => "agent_memory",
            Self::Binary => "binary",
            Self::Unknown => "unknown",
        }
    }
}

impl TryFrom<&str> for FileKind {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "source" => Ok(Self::Source),
            "text" => Ok(Self::Text),
            "semi_structured" => Ok(Self::SemiStructured),
            "tabular" => Ok(Self::Tabular),
            "agent_instructions" => Ok(Self::AgentInstructions),
            "agent_skill" => Ok(Self::AgentSkill),
            "agent_memory" => Ok(Self::AgentMemory),
            "binary" => Ok(Self::Binary),
            "unknown" => Ok(Self::Unknown),
            other => anyhow::bail!("unknown file kind: {other}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexOptions {
    pub max_content_bytes: u64,
    pub max_profile_bytes: u64,
    pub duckdb_timeout_seconds: u64,
}

impl Default for IndexOptions {
    fn default() -> Self {
        Self {
            max_content_bytes: 4 * 1024 * 1024,
            max_profile_bytes: 256 * 1024 * 1024,
            duckdb_timeout_seconds: 15,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ExistingFile {
    pub id: i64,
    pub size_bytes: u64,
    pub mtime_ns: i64,
    pub content_hash: Option<String>,
    pub kind: FileKind,
    pub generation_completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolRecord {
    pub name: String,
    pub kind: String,
    pub language: String,
    pub line_start: usize,
    pub line_end: usize,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetColumn {
    pub name: String,
    pub data_type: String,
    pub nullable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatasetProfile {
    pub format: String,
    pub status: String,
    pub columns: Vec<DatasetColumn>,
    pub profiler: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDocumentRole {
    Instructions,
    Skill,
}

impl AgentDocumentRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Instructions => "instructions",
            Self::Skill => "skill",
        }
    }
}

impl TryFrom<&str> for AgentDocumentRole {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "instructions" => Ok(Self::Instructions),
            "skill" => Ok(Self::Skill),
            other => anyhow::bail!("unknown Agent document role: {other}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDocumentMetadata {
    pub role: AgentDocumentRole,
    pub name: Option<String>,
    pub description: Option<String>,
    pub scope_root: PathBuf,
    pub precedence_depth: usize,
    pub headings: Vec<String>,
    pub references: Vec<PathBuf>,
}

impl AgentDocumentMetadata {
    pub fn search_text(&self) -> String {
        let mut parts = vec![self.role.as_str().to_owned()];
        parts.extend(self.name.iter().cloned());
        parts.extend(self.description.iter().cloned());
        parts.extend(self.headings.iter().cloned());
        parts.extend(
            self.references
                .iter()
                .map(|path| path.to_string_lossy().into_owned()),
        );
        parts.join(" ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMemoryLayer {
    UserProfile,
    ProjectSummary,
    TopicSummary,
    SessionSummary,
    MemoryNote,
    RawHistory,
}

impl AgentMemoryLayer {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserProfile => "user_profile",
            Self::ProjectSummary => "project_summary",
            Self::TopicSummary => "topic_summary",
            Self::SessionSummary => "session_summary",
            Self::MemoryNote => "memory_note",
            Self::RawHistory => "raw_history",
        }
    }

    pub fn summary_priority(self) -> u8 {
        match self {
            Self::ProjectSummary => 5,
            Self::TopicSummary => 4,
            Self::UserProfile => 3,
            Self::SessionSummary => 2,
            Self::MemoryNote => 1,
            Self::RawHistory => 0,
        }
    }
}

impl TryFrom<&str> for AgentMemoryLayer {
    type Error = anyhow::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "user_profile" => Ok(Self::UserProfile),
            "project_summary" => Ok(Self::ProjectSummary),
            "topic_summary" => Ok(Self::TopicSummary),
            "session_summary" => Ok(Self::SessionSummary),
            "memory_note" => Ok(Self::MemoryNote),
            "raw_history" => Ok(Self::RawHistory),
            other => anyhow::bail!("unknown Agent memory layer: {other}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMemorySource {
    pub path: PathBuf,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_key: Option<String>,
    pub raw_history: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMemoryMetadata {
    pub agent: String,
    pub layer: AgentMemoryLayer,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub observed_at_ms: i64,
    pub raw_history: bool,
}

impl AgentMemoryMetadata {
    pub fn search_text(&self) -> String {
        [
            Some(self.agent.as_str()),
            Some(self.layer.as_str()),
            self.name.as_deref(),
            self.description.as_deref(),
            self.workspace_root.as_ref().and_then(|path| path.to_str()),
            self.project_key.as_deref(),
            self.session_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentMemoryIndexReport {
    pub project_root: PathBuf,
    pub include_raw: bool,
    pub sources: Vec<AgentMemorySource>,
    pub reports: Vec<IndexReport>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentMemoryRefreshReport {
    pub projects: u64,
    pub sources: u64,
    pub indexed: u64,
    pub deleted: u64,
}

impl DatasetProfile {
    pub fn schema_text(&self) -> String {
        self.columns
            .iter()
            .map(|column| format!("{} {}", column.name, column.data_type))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CatalogFileInput {
    pub root_id: i64,
    pub absolute_path: String,
    pub relative_path: String,
    pub name: String,
    pub extension: Option<String>,
    pub kind: FileKind,
    pub experiment: Option<String>,
    pub size_bytes: u64,
    pub mtime_ns: i64,
    pub content_hash: Option<String>,
    pub generation: i64,
    pub content_indexed: bool,
    pub extraction_status: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SearchDocument {
    pub file_id: i64,
    pub path: String,
    pub name: String,
    pub experiment: String,
    pub kind: String,
    pub content: String,
    pub symbols: String,
    pub schema: String,
    pub preview: String,
    pub generation: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticCandidate {
    pub file_id: i64,
    pub path: String,
    pub generation: i64,
    pub score: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SemanticBuildReport {
    pub generation: i64,
    pub files: u64,
    pub reused_files: u64,
    pub chunks: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct LaneEvidence {
    pub name: String,
    pub score: f32,
    pub weight: f32,
    pub contribution: f32,
}

#[derive(Debug, Clone)]
pub(crate) struct SearchCandidate {
    pub file_id: i64,
    pub path: String,
    pub generation: i64,
    pub score: f32,
    pub lanes: Vec<String>,
    pub evidence: Vec<LaneEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub file_id: i64,
    pub path: String,
    pub relative_path: String,
    pub kind: FileKind,
    pub experiment: Option<String>,
    pub size_bytes: u64,
    pub mtime_ns: i64,
    pub generation: i64,
    pub score: f32,
    pub matched_lanes: Vec<String>,
    #[serde(skip)]
    pub(crate) evidence: Vec<LaneEvidence>,
    pub preview: String,
    #[serde(skip)]
    pub(crate) content_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_context: Option<SourceContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<SymbolRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentDocumentMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<AgentMemoryMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceContext {
    pub scope_root: String,
    pub in_scope: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<SourceAlternative>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceAlternative {
    pub path: String,
    pub relationship: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileRecord {
    pub id: i64,
    pub root: String,
    pub absolute_path: String,
    pub relative_path: String,
    pub name: String,
    pub extension: Option<String>,
    pub kind: FileKind,
    pub experiment: Option<String>,
    pub size_bytes: u64,
    pub mtime_ns: i64,
    pub content_hash: Option<String>,
    pub generation: i64,
    pub content_indexed: bool,
    pub extraction_status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectResult {
    pub file: FileRecord,
    pub symbols: Vec<SymbolRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused_symbol: Option<SymbolRecord>,
    pub dataset: Option<DatasetProfile>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentDocumentMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<AgentMemoryMetadata>,
    pub content: Option<ContentExcerpt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentExcerpt {
    pub start_line: usize,
    pub end_line: usize,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryInput {
    pub path: PathBuf,
    pub alias: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRequest {
    pub sql: String,
    pub roots: Vec<PathBuf>,
    pub inputs: Vec<QueryInput>,
    pub max_rows: usize,
    pub max_bytes: usize,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryColumn {
    pub name: String,
    pub data_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuerySource {
    pub alias: String,
    pub path: String,
    pub format: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<QueryColumn>,
    pub rows: Vec<Vec<Value>>,
    pub sources: Vec<QuerySource>,
    pub row_count: usize,
    pub truncated: bool,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexReport {
    pub root: PathBuf,
    pub generation: i64,
    pub discovered: u64,
    pub indexed: u64,
    pub unchanged: u64,
    pub deleted: u64,
    pub metadata_only: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NotifyReport {
    pub requested: u64,
    pub roots: Vec<IndexReport>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IndexStatus {
    /// "initial" | "loading" | "populated" | "error". Exposes whether the rich
    /// fields below come from a real scan or are still a cold/failed default,
    /// so an all-zero response never masquerades as a healthy index.
    #[serde(default)]
    pub status_state: String,
    /// Machine-readable reason when `status_state == "error"`; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_error: Option<String>,
    /// Wall-clock (unix ms) when the background refresher last computed this body,
    /// so a client can tell a fresh populated snapshot from a stale cached one.
    #[serde(default)]
    pub status_sampled_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shards: Vec<ShardStatus>,
    pub completed_generation: Option<i64>,
    pub running_generations: u64,
    pub failed_generations: u64,
    pub active_files: u64,
    pub deleted_files: u64,
    pub symbols: u64,
    pub datasets: u64,
    #[serde(default)]
    pub agent_documents: u64,
    #[serde(default)]
    pub agent_memories: u64,
    #[serde(default)]
    pub memory: MemoryStatus,
    pub failures: u64,
    #[serde(default)]
    pub semantic: SemanticStatus,
    #[serde(default)]
    pub retrieval: RetrievalStatus,
    #[serde(default)]
    pub serving: ServingStatus,
    #[serde(default)]
    pub freshness: FreshnessStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardStatus {
    pub name: String,
    pub index_dir: PathBuf,
    #[serde(flatten)]
    pub visibility: crate::collection::Visibility,
    pub status: IndexStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryStatus {
    pub registered_projects: u64,
    pub registered_sources: u64,
    pub active_files: u64,
    pub generation: Option<i64>,
    pub generation_lag: u64,
    pub stale_files: u64,
    pub missing_files: u64,
    pub max_source_lag_ms: u64,
    pub oldest_source_age_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SemanticStatus {
    pub enabled: bool,
    pub available: bool,
    #[serde(default)]
    pub runtime_ready: bool,
    #[serde(default)]
    pub startup_failures: u64,
    #[serde(default)]
    pub effective_mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub generation: Option<i64>,
    pub files: u64,
    pub chunks: u64,
    #[serde(default)]
    pub chunking_version: u64,
    #[serde(default)]
    pub generation_lag: u64,
    #[serde(default)]
    pub metrics: SemanticMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LatencySummary {
    pub samples: u64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SemanticMetrics {
    pub queries: u64,
    pub result_cache_hits: u64,
    pub embedding_cache_hits: u64,
    pub coalesced_queries: u64,
    pub failures: u64,
    pub sidecar_starts: u64,
    pub sidecar_restarts: u64,
    #[serde(default)]
    pub startup_failures: u64,
    pub queue_wait: LatencySummary,
    #[serde(default)]
    pub sidecar_startup: LatencySummary,
    pub embedding: LatencySummary,
    #[serde(default)]
    pub snapshot_open: LatencySummary,
    #[serde(default)]
    pub serialization: LatencySummary,
    pub vector_search: LatencySummary,
    pub total: LatencySummary,
    /// Real LanceDB snapshot-open events (connect + open_table) counted across
    /// the process. The `snapshot_open` window holds exactly these samples;
    /// cached-handle queries do not contribute. This is the semantic sub-stage;
    /// the reader-level snapshot open stage is reported by the daemon layer.
    #[serde(default)]
    pub snapshot_opened: u64,
}

/// Structured index freshness.
///
/// `last_published_ms` is the mtime of the newest published materialization
/// (the semantic manifest / catalog snapshot) the reader currently serves.
/// For a collection (fast + nfs shards) the aggregate takes the *stalest*
/// shard's age as the system freshness, so a single slow shard cannot be
/// masked by a fast one; per-shard ages are reported inside `shards`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FreshnessStatus {
    #[serde(default)]
    pub now_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_published_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_ms: Option<u64>,
    /// Per-shard last-published timestamps, so the fast/nfs aggregation is
    /// auditable rather than a single opaque number. Source: the authoritative
    /// `created_at_ms` recorded in each shard's published `manifest.json` at
    /// publish time — never the index dir or semantic-manifest mtime.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shard_last_published_ms: std::collections::BTreeMap<String, u64>,
    /// Per-shard authoritative publication generation being served, keyed by shard
    /// name. Generations from different shards are NOT comparable numbers, so this
    /// map is the only auditable per-shard generation; the system-level field below
    /// is derived from publication TIME, not by min/max-ing generation counters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub shard_published_generation: std::collections::BTreeMap<String, i64>,
    /// System-level publication generation: the generation of the shard whose
    /// publication time is the OLDEST currently being served (the system is only
    /// as fresh as its stalest publish). `published_generation_shard` names it.
    /// Null when no shard publication metadata is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_generation: Option<i64>,
    /// Which shard `published_generation` was taken from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_generation_shard: Option<String>,
    /// Producer's last completed scan time (`scans.last_completed_ms`) from the
    /// bundled producer-state.sqlite3. This is a SCAN completion timestamp, NOT a
    /// liveness heartbeat; it cannot by itself prove the producer is alive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_completed_ms: Option<u64>,
    /// Real producer liveness heartbeat from the producer's health endpoint/JSON,
    /// when one is published. Null (default) when absent — never synthesized from
    /// scan timestamps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heartbeat_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetrievalStatus {
    pub searches: u64,
    pub in_flight: u64,
    pub cache_hits: u64,
    pub semantic_fallbacks: u64,
    pub total: LatencySummary,
    pub lexical: LatencySummary,
    pub semantic: LatencySummary,
    pub fusion: LatencySummary,
    pub catalog_filter: LatencySummary,
    pub preview: LatencySummary,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServingStatus {
    pub workers: u64,
    pub queue_capacity: u64,
    pub accepted: u64,
    pub completed: u64,
    pub rejected: u64,
    pub active: u64,
    pub queued: u64,
    pub queue_wait: LatencySummary,
    /// Real reader snapshot-open events (worker startup + rotation). Warm
    /// reused readers contribute no samples; this comes from the shared worker
    /// metrics, never from a fresh ad-hoc ReaderIndex.
    #[serde(default)]
    pub reader_snapshot_open: LatencySummary,
    /// Real on-wire response serialization cost (the single `serde_json::to_vec`
    /// whose bytes are written to the socket). Recorded post-write to the shared
    /// worker metrics; aggregate, not per-query.
    #[serde(default)]
    pub response_serialization: LatencySummary,
}
