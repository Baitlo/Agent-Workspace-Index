pub mod benchmark;
mod catalog;
mod change_ledger;
pub mod collection;
pub mod daemon;
mod data;
mod diagnostics;
mod extract;
pub mod fence;
pub mod integration;
pub mod mcp;
mod mcp_audit;
pub mod memory;
mod metrics;
mod model;
pub mod producer;
pub mod protocol;
pub mod publisher;
mod reader;
mod search;
mod semantic;
pub mod snapshot;
mod source_context;
mod workspace;

pub use integration::{
    ClientIntegration, IntegrationClient, IntegrationOptions, IntegrationReport, IntegrationStatus,
    McpServerSpec, default_server_spec, integrate, render_human,
};
pub use memory::discover_agent_memory_sources;
pub use model::{
    AgentDocumentMetadata, AgentDocumentRole, AgentMemoryIndexReport, AgentMemoryLayer,
    AgentMemoryMetadata, AgentMemorySource, ContentExcerpt, DatasetColumn, DatasetProfile,
    FileKind, FileRecord, IndexOptions, IndexReport, IndexStatus, InspectResult, LatencySummary,
    NotifyReport, QueryColumn, QueryInput, QueryRequest, QueryResult, QuerySource, RetrievalStatus,
    SearchHit, SemanticBuildReport, SemanticCandidate, SemanticMetrics, SemanticStatus,
    ServingStatus, SymbolRecord,
};
pub use publisher::{
    PublishCycle, PublisherConfig, partition_roots, publish_once, resolve_roots, watch,
};
pub use workspace::WorkspaceIndex;
