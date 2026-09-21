pub mod benchmark;
mod catalog;
pub mod daemon;
mod data;
mod extract;
pub mod integration;
pub mod mcp;
mod mcp_audit;
mod model;
pub mod protocol;
pub mod publisher;
mod search;
pub mod snapshot;
mod workspace;

pub use integration::{
    ClientIntegration, IntegrationClient, IntegrationOptions, IntegrationReport, IntegrationStatus,
    McpServerSpec, default_server_spec, integrate, render_human,
};
pub use model::{
    AgentDocumentMetadata, AgentDocumentRole, ContentExcerpt, DatasetColumn, DatasetProfile,
    FileKind, FileRecord, IndexOptions, IndexReport, IndexStatus, InspectResult, NotifyReport,
    QueryColumn, QueryInput, QueryRequest, QueryResult, QuerySource, SearchHit, SymbolRecord,
};
pub use publisher::{
    PublishCycle, PublisherConfig, partition_roots, publish_once, resolve_roots, watch,
};
pub use workspace::WorkspaceIndex;
