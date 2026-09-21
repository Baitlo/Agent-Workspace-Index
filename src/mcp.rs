use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig},
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::daemon::try_request;
use crate::mcp_audit::{McpAuditLogger, McpAuditSpan};
use crate::protocol::Request;
use crate::{InspectResult, QueryInput, QueryRequest, QueryResult, SearchHit, WorkspaceIndex};

const DEFAULT_SEARCH_LIMIT: usize = 10;
const MAX_SEARCH_LIMIT: usize = 50;
const MAX_QUERY_CHARS: usize = 4_096;
const MAX_PREVIEW_CHARS: usize = 2_000;
const DEFAULT_SYMBOL_LIMIT: usize = 200;
const MAX_SYMBOL_LIMIT: usize = 1_000;
const DEFAULT_INSPECT_LINES: usize = 120;
const MAX_INSPECT_LINES: usize = 500;
const DEFAULT_INSPECT_CHARS: usize = 32 * 1024;
const MAX_INSPECT_CHARS: usize = 64 * 1024;
const MAX_DATASET_COLUMNS: usize = 500;
const MAX_DATASET_COLUMN_NAME_CHARS: usize = 256;
const MAX_DATASET_TYPE_CHARS: usize = 512;
const MAX_DATASET_SCHEMA_CHARS: usize = 16 * 1024;
const MAX_STRUCTURED_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const DEFAULT_QUERY_ROWS: usize = 100;
const MAX_MCP_QUERY_ROWS: usize = 1_000;
const DEFAULT_QUERY_BYTES: usize = 1024 * 1024;
const DEFAULT_QUERY_TIMEOUT_MS: u64 = 10_000;
const MAX_QUERY_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceSearchRequest {
    /// Terms, path fragments, symbol names, or dataset columns to find.
    #[schemars(length(min = 1, max = 4_096))]
    pub query: String,
    /// Maximum number of ranked results. Defaults to 10 and cannot exceed 50.
    #[schemars(schema_with = "optional_integer_schema", range(min = 1, max = 50))]
    pub limit: Option<usize>,
    /// Optional search scopes. A registered root or an existing parent directory
    /// containing one or more registered roots is accepted.
    pub roots: Option<Vec<PathBuf>>,
    /// Optional file kinds. Use source for code; text for SQL/Markdown/logs;
    /// agent_instructions for AGENTS.md; agent_skill for SKILL.md;
    /// agent_memory for project-scoped cross-Agent memory;
    /// semi_structured for .json; tabular for .csv/.tsv/.jsonl/.ndjson/.parquet.
    /// Omit this filter when the file kind is uncertain.
    pub kinds: Option<Vec<String>>,
    /// Optional absolute or root-relative path prefix.
    pub path_prefix: Option<String>,
    /// Optional workspace file or directory whose applicable AGENTS.md hierarchy
    /// should be included and ranked from broadest to nearest scope.
    pub context_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceInspectRequest {
    /// Absolute or current-workspace-relative path already present in the index.
    pub path: PathBuf,
    /// Maximum number of symbols to return. Defaults to 200 and cannot exceed 1000.
    #[schemars(schema_with = "optional_integer_schema", range(min = 1, max = 1_000))]
    pub max_symbols: Option<usize>,
    /// First one-based source line to return. Defaults to 1.
    #[schemars(schema_with = "optional_integer_schema", range(min = 1))]
    pub start_line: Option<usize>,
    /// Maximum source lines to return. Defaults to 120 and cannot exceed 500.
    #[schemars(schema_with = "optional_integer_schema", range(min = 1, max = 500))]
    pub max_lines: Option<usize>,
    /// Maximum source characters to return. Defaults to 32768 and cannot exceed 65536.
    #[schemars(schema_with = "optional_integer_schema", range(min = 1, max = 65_536))]
    pub max_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceQueryInput {
    /// CSV, TSV, JSON, JSONL, NDJSON, or Parquet file beneath an allowed root.
    pub path: PathBuf,
    /// SQL relation name. Defaults to data_0, data_1, and so on.
    pub alias: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceQueryRequest {
    /// One read-only SELECT or WITH query over the registered input aliases.
    /// Double-quote column names that are SQL keywords, for example "window".
    pub sql: String,
    /// Canonical directory allowlist for all input files.
    pub roots: Vec<PathBuf>,
    /// Explicit input files exposed as SQL relations.
    pub inputs: Vec<WorkspaceQueryInput>,
    /// Maximum returned rows. Defaults to 100 and cannot exceed 1000.
    #[schemars(schema_with = "optional_integer_schema", range(min = 1, max = 1_000))]
    pub max_rows: Option<usize>,
    /// Maximum serialized result bytes. Defaults to 1 MiB and cannot exceed 2 MiB.
    #[schemars(
        schema_with = "optional_integer_schema",
        range(min = 1_024, max = 2_097_152)
    )]
    pub max_bytes: Option<usize>,
    /// Query timeout in milliseconds. Defaults to 10 seconds and cannot exceed 30 seconds.
    #[schemars(schema_with = "optional_integer_schema", range(min = 1, max = 30_000))]
    pub timeout_ms: Option<u64>,
}

fn optional_integer_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "type": ["integer", "null"]
    })
}

#[derive(Debug, Clone)]
pub struct AwiMcpServer {
    index_dir: PathBuf,
    socket_path: PathBuf,
    audit_logger: Option<Arc<McpAuditLogger>>,
    tool_router: ToolRouter<Self>,
}

impl AwiMcpServer {
    pub fn new(index_dir: PathBuf, socket_path: PathBuf) -> Self {
        Self {
            index_dir,
            socket_path,
            audit_logger: None,
            tool_router: Self::tool_router(),
        }
    }

    fn with_audit_log(
        index_dir: PathBuf,
        socket_path: PathBuf,
        audit_log: Option<&Path>,
    ) -> Result<Self> {
        let audit_logger = audit_log.map(McpAuditLogger::open).transpose()?;
        Ok(Self {
            index_dir,
            socket_path,
            audit_logger,
            tool_router: Self::tool_router(),
        })
    }

    fn audit_span(&self, tool: &'static str, arguments: Value) -> Option<McpAuditSpan> {
        self.audit_logger
            .as_ref()
            .map(|logger| logger.span(tool, arguments))
    }

    async fn execute(&self, request: Request) -> Result<Value> {
        let index_dir = self.index_dir.clone();
        let socket_path = self.socket_path.clone();
        tokio::task::spawn_blocking(move || execute_request(&index_dir, &socket_path, request))
            .await
            .context("join AWI MCP request worker")?
    }
}

#[tool_router]
impl AwiMcpServer {
    /// Preferred first step for locating code, symbols, docs, or dataset schemas in
    /// an indexed workspace: use this before shell grep or file walking. Hybrid
    /// ranking over indexed paths, source text, symbols, and dataset columns. Each
    /// non-empty preview is a direct excerpt from the same indexed generation; use
    /// it as evidence and inspect only when the required detail is absent.
    #[tool(
        name = "workspace_search",
        annotations(
            title = "Workspace Search",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn workspace_search(
        &self,
        Parameters(arguments): Parameters<WorkspaceSearchRequest>,
    ) -> Result<CallToolResult, McpError> {
        let audit = self.audit_span(
            "workspace_search",
            json!({
                "query": &arguments.query,
                "limit": arguments.limit,
                "roots": &arguments.roots,
                "kinds": &arguments.kinds,
                "path_prefix": &arguments.path_prefix,
                "context_path": &arguments.context_path
            }),
        );
        let query = arguments.query.trim();
        if query.is_empty() {
            return audited(
                audit,
                Err(McpError::invalid_params("query must not be empty", None)),
            );
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return audited(
                audit,
                Err(McpError::invalid_params(
                    format!("query cannot exceed {MAX_QUERY_CHARS} characters"),
                    None,
                )),
            );
        }

        let limit = arguments.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        if !(1..=MAX_SEARCH_LIMIT).contains(&limit) {
            return audited(
                audit,
                Err(McpError::invalid_params(
                    format!("limit must be between 1 and {MAX_SEARCH_LIMIT}"),
                    None,
                )),
            );
        }

        let value = match self
            .execute(Request::Search {
                query: query.to_owned(),
                limit,
                roots: arguments.roots.unwrap_or_default(),
                kinds: arguments.kinds.unwrap_or_default(),
                path_prefix: arguments.path_prefix,
                context_path: arguments.context_path,
            })
            .await
        {
            Ok(value) => value,
            Err(error) => {
                return audited(audit, Ok(tool_error("search_failed", &error)));
            }
        };
        let mut hits: Vec<SearchHit> = match serde_json::from_value(value) {
            Ok(hits) => hits,
            Err(error) => {
                return audited(
                    audit,
                    Ok(tool_error("invalid_search_response", &error.into())),
                );
            }
        };
        let mut previews_truncated = false;
        for hit in &mut hits {
            previews_truncated |= truncate_chars(&mut hit.preview, MAX_PREVIEW_CHARS);
        }

        audited(
            audit,
            Ok(bounded_result(json!({
                "hits": hits,
                "previews_truncated": previews_truncated
            }))),
        )
    }

    /// Inspect metadata, symbols, schema, and a bounded line-numbered text excerpt.
    /// For long text with facts spread across the file, request up to 500 lines in the
    /// first call instead of paging through several smaller excerpts.
    #[tool(
        name = "workspace_inspect",
        annotations(
            title = "Workspace Inspect",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn workspace_inspect(
        &self,
        Parameters(arguments): Parameters<WorkspaceInspectRequest>,
    ) -> Result<CallToolResult, McpError> {
        let audit = self.audit_span(
            "workspace_inspect",
            json!({
                "path": &arguments.path,
                "max_symbols": arguments.max_symbols,
                "start_line": arguments.start_line,
                "max_lines": arguments.max_lines,
                "max_chars": arguments.max_chars
            }),
        );
        let max_symbols = arguments.max_symbols.unwrap_or(DEFAULT_SYMBOL_LIMIT);
        if !(1..=MAX_SYMBOL_LIMIT).contains(&max_symbols) {
            return audited(
                audit,
                Err(McpError::invalid_params(
                    format!("max_symbols must be between 1 and {MAX_SYMBOL_LIMIT}"),
                    None,
                )),
            );
        }
        let start_line = arguments.start_line.unwrap_or(1);
        if start_line == 0 {
            return audited(
                audit,
                Err(McpError::invalid_params(
                    "start_line must be positive",
                    None,
                )),
            );
        }
        let max_lines = arguments.max_lines.unwrap_or(DEFAULT_INSPECT_LINES);
        if !(1..=MAX_INSPECT_LINES).contains(&max_lines) {
            return audited(
                audit,
                Err(McpError::invalid_params(
                    format!("max_lines must be between 1 and {MAX_INSPECT_LINES}"),
                    None,
                )),
            );
        }
        let max_chars = arguments.max_chars.unwrap_or(DEFAULT_INSPECT_CHARS);
        if !(1..=MAX_INSPECT_CHARS).contains(&max_chars) {
            return audited(
                audit,
                Err(McpError::invalid_params(
                    format!("max_chars must be between 1 and {MAX_INSPECT_CHARS}"),
                    None,
                )),
            );
        }

        let value = match self
            .execute(Request::Inspect {
                path: arguments.path,
                start_line,
                max_lines,
                max_chars,
            })
            .await
        {
            Ok(value) => value,
            Err(error) => {
                return audited(audit, Ok(tool_error("inspect_failed", &error)));
            }
        };
        let mut result: InspectResult = match serde_json::from_value(value) {
            Ok(result) => result,
            Err(error) => {
                return audited(
                    audit,
                    Ok(tool_error("invalid_inspect_response", &error.into())),
                );
            }
        };
        let total_symbols = result.symbols.len();
        result.symbols.truncate(max_symbols);

        let total_columns = result
            .dataset
            .as_ref()
            .map_or(0, |dataset| dataset.columns.len());
        let mut dataset_schema_truncated = false;
        if let Some(dataset) = &mut result.dataset {
            dataset.columns.truncate(MAX_DATASET_COLUMNS);
            let mut retained = 0usize;
            let mut schema_chars = 0usize;
            for column in &mut dataset.columns {
                dataset_schema_truncated |=
                    truncate_chars(&mut column.name, MAX_DATASET_COLUMN_NAME_CHARS);
                dataset_schema_truncated |=
                    truncate_chars(&mut column.data_type, MAX_DATASET_TYPE_CHARS);
                let column_chars =
                    column.name.chars().count() + column.data_type.chars().count() + 1;
                if schema_chars.saturating_add(column_chars) > MAX_DATASET_SCHEMA_CHARS {
                    dataset_schema_truncated = true;
                    break;
                }
                schema_chars += column_chars;
                retained += 1;
            }
            dataset_schema_truncated |= retained < dataset.columns.len();
            dataset.columns.truncate(retained);
        }

        audited(
            audit,
            Ok(bounded_result(json!({
                "file": result.file,
                "symbols": result.symbols,
                "dataset": result.dataset,
                "agent": result.agent,
                "content": result.content,
                "coverage": {
                    "total_symbols": total_symbols,
                    "symbols_truncated": total_symbols > max_symbols,
                    "total_dataset_columns": total_columns,
                    "dataset_columns_truncated": total_columns > MAX_DATASET_COLUMNS,
                    "dataset_schema_truncated": dataset_schema_truncated
                }
            }))),
        )
    }

    /// Execute bounded read-only DuckDB SQL over explicitly allowed structured files.
    #[tool(
        name = "workspace_query",
        annotations(
            title = "Workspace Query",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn workspace_query(
        &self,
        Parameters(arguments): Parameters<WorkspaceQueryRequest>,
    ) -> Result<CallToolResult, McpError> {
        let audit = self.audit_span(
            "workspace_query",
            json!({
                "sql": &arguments.sql,
                "roots": &arguments.roots,
                "inputs": &arguments.inputs.iter().map(|input| {
                    json!({"path": &input.path, "alias": &input.alias})
                }).collect::<Vec<_>>(),
                "max_rows": arguments.max_rows,
                "max_bytes": arguments.max_bytes,
                "timeout_ms": arguments.timeout_ms
            }),
        );
        let max_rows = arguments.max_rows.unwrap_or(DEFAULT_QUERY_ROWS);
        if !(1..=MAX_MCP_QUERY_ROWS).contains(&max_rows) {
            return audited(
                audit,
                Err(McpError::invalid_params(
                    format!("max_rows must be between 1 and {MAX_MCP_QUERY_ROWS}"),
                    None,
                )),
            );
        }
        let max_bytes = arguments.max_bytes.unwrap_or(DEFAULT_QUERY_BYTES);
        if !(1_024..=MAX_STRUCTURED_RESPONSE_BYTES).contains(&max_bytes) {
            return audited(
                audit,
                Err(McpError::invalid_params(
                    format!("max_bytes must be between 1024 and {MAX_STRUCTURED_RESPONSE_BYTES}"),
                    None,
                )),
            );
        }
        let timeout_ms = arguments.timeout_ms.unwrap_or(DEFAULT_QUERY_TIMEOUT_MS);
        if !(1..=MAX_QUERY_TIMEOUT_MS).contains(&timeout_ms) {
            return audited(
                audit,
                Err(McpError::invalid_params(
                    format!("timeout_ms must be between 1 and {MAX_QUERY_TIMEOUT_MS}"),
                    None,
                )),
            );
        }

        let request = QueryRequest {
            sql: arguments.sql,
            roots: arguments.roots,
            inputs: arguments
                .inputs
                .into_iter()
                .map(|input| QueryInput {
                    path: input.path,
                    alias: input.alias,
                })
                .collect(),
            max_rows,
            max_bytes,
            timeout_ms,
        };
        let value = match self.execute(Request::Query { request }).await {
            Ok(value) => value,
            Err(error) => {
                return audited(audit, Ok(tool_error("query_failed", &error)));
            }
        };
        let result: QueryResult = match serde_json::from_value(value) {
            Ok(result) => result,
            Err(error) => {
                return audited(
                    audit,
                    Ok(tool_error("invalid_query_response", &error.into())),
                );
            }
        };
        audited(audit, Ok(bounded_result(json!(result))))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AwiMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(
                Implementation::new("awi", env!("CARGO_PKG_VERSION"))
                    .with_title("AWI: Agent Workspace Index")
                    .with_description(
                        "Hybrid retrieval over indexed workspace code, data, Agent knowledge, and memory",
                    ),
            )
            .with_instructions(
                "For any code, symbol, document, Agent instruction, skill, memory, or dataset lookup inside \
                 an indexed workspace, use workspace_search first, before shell grep or file \
                 walking. Pass context_path when resolving applicable AGENTS.md instructions or \
                 project-scoped Agent memory. \
                 Non-empty previews are direct excerpts from the indexed generation and are \
                 sufficient evidence when they contain the required facts. Once an authoritative \
                 path is selected, do not repeat discovery searches. Use one workspace_inspect \
                 call, with up to 500 lines for long text, only for missing details; use \
                 workspace_query directly for structured aggregation. If a path is not in an \
                 indexed root, fall back to ordinary file tools instead of passing it here.",
            )
    }
}

pub fn serve_stdio(
    index_dir: PathBuf,
    socket_path: PathBuf,
    audit_log: Option<PathBuf>,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build AWI MCP runtime")?;
    runtime.block_on(async move {
        let service = AwiMcpServer::with_audit_log(index_dir, socket_path, audit_log.as_deref())?
            .serve(rmcp::transport::stdio())
            .await
            .context("start AWI MCP stdio service")?;
        service
            .waiting()
            .await
            .context("run AWI MCP stdio service")
            .map(|_| ())
    })
}

fn audited(
    audit: Option<McpAuditSpan>,
    result: Result<CallToolResult, McpError>,
) -> Result<CallToolResult, McpError> {
    if let Some(audit) = audit {
        audit.finish(&result);
    }
    result
}

fn execute_request(index_dir: &Path, socket_path: &Path, request: Request) -> Result<Value> {
    if let Some(value) = try_request(socket_path, &request)? {
        return Ok(value);
    }

    let workspace = WorkspaceIndex::open(index_dir)
        .with_context(|| format!("open AWI index {}", index_dir.display()))?;
    match request {
        Request::Search {
            query,
            limit,
            roots,
            kinds,
            path_prefix,
            context_path,
        } => serde_json::to_value(workspace.search_filtered(
            &query,
            limit,
            &roots,
            &kinds,
            path_prefix.as_deref(),
            context_path.as_deref(),
        )?)
        .map_err(Into::into),
        Request::Inspect {
            path,
            start_line,
            max_lines,
            max_chars,
        } => {
            serde_json::to_value(workspace.inspect_excerpt(path, start_line, max_lines, max_chars)?)
                .map_err(Into::into)
        }
        Request::Query { request } => {
            serde_json::to_value(workspace.query(&request)?).map_err(Into::into)
        }
        _ => anyhow::bail!("unsupported MCP workspace request"),
    }
}

fn tool_error(code: &str, error: &anyhow::Error) -> CallToolResult {
    CallToolResult::structured_error(json!({
        "error": {
            "code": code,
            "message": format!("{error:#}")
        }
    }))
}

fn bounded_result(value: Value) -> CallToolResult {
    match serde_json::to_vec(&value) {
        Ok(encoded) if encoded.len() <= MAX_STRUCTURED_RESPONSE_BYTES => {
            CallToolResult::structured(value)
        }
        Ok(encoded) => CallToolResult::structured_error(json!({
            "error": {
                "code": "response_too_large",
                "message": format!(
                    "response is {} bytes; maximum is {MAX_STRUCTURED_RESPONSE_BYTES} bytes",
                    encoded.len()
                )
            }
        })),
        Err(error) => tool_error("response_serialization_failed", &error.into()),
    }
}

fn truncate_chars(value: &mut String, max_chars: usize) -> bool {
    let Some((byte_index, _)) = value.char_indices().nth(max_chars) else {
        return false;
    };
    value.truncate(byte_index);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_utf8_at_character_boundary() {
        let mut value = "ab公式cd".to_owned();
        assert!(truncate_chars(&mut value, 3));
        assert_eq!(value, "ab公");
        assert!(!truncate_chars(&mut value, 3));
    }

    #[test]
    fn rejects_oversized_structured_result() {
        let result = bounded_result(json!({
            "payload": "x".repeat(MAX_STRUCTURED_RESPONSE_BYTES)
        }));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            result.structured_content.unwrap()["error"]["code"],
            "response_too_large"
        );
    }

    #[test]
    fn numeric_tool_parameters_use_portable_integer_schemas() {
        for schema in [
            schemars::schema_for!(WorkspaceSearchRequest),
            schemars::schema_for!(WorkspaceInspectRequest),
            schemars::schema_for!(WorkspaceQueryRequest),
        ] {
            let encoded = serde_json::to_value(schema).unwrap();
            assert!(
                !encoded.to_string().contains("\"format\":\"uint"),
                "MCP schema must not expose Rust-specific unsigned integer formats"
            );
        }
    }
}
