pub mod benchmark;
mod catalog;
pub mod daemon;
mod data;
mod extract;
pub mod integration;
pub mod mcp;
mod mcp_audit;
pub mod memory;
mod model;
pub mod protocol;
pub mod publisher;
mod search;
mod semantic;
pub mod snapshot;
mod workspace;

pub use integration::{
    ClientIntegration, IntegrationClient, IntegrationOptions, IntegrationReport, IntegrationStatus,
    McpServerSpec, default_server_spec, integrate, render_human,
};
pub use memory::discover_agent_memory_sources;
pub use model::{
    AgentDocumentMetadata, AgentDocumentRole, AgentMemoryIndexReport, AgentMemoryLayer,
    AgentMemoryMetadata, AgentMemorySource, ContentExcerpt, DatasetColumn, DatasetProfile,
    FileKind, FileRecord, IndexOptions, IndexReport, IndexStatus, InspectResult, NotifyReport,
    QueryColumn, QueryInput, QueryRequest, QueryResult, QuerySource, SearchHit,
    SemanticBuildReport, SemanticCandidate, SemanticStatus, SymbolRecord,
};
pub use publisher::{
    PublishCycle, PublisherConfig, partition_roots, publish_once, resolve_roots, watch,
};
pub use workspace::WorkspaceIndex;
