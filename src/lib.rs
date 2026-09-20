pub mod benchmark;
mod catalog;
pub mod daemon;
mod data;
mod extract;
pub mod mcp;
mod mcp_audit;
mod model;
pub mod protocol;
mod search;
pub mod snapshot;
mod workspace;

pub use model::{
    ContentExcerpt, DatasetColumn, DatasetProfile, FileKind, FileRecord, IndexOptions, IndexReport,
    IndexStatus, InspectResult, NotifyReport, QueryColumn, QueryInput, QueryRequest, QueryResult,
    QuerySource, SearchHit, SymbolRecord,
};
pub use workspace::WorkspaceIndex;
