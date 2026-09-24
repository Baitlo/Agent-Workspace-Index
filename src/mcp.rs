use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rmcp::{
    ErrorData as McpError, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities,
        ServerConfig,
    },
    schemars, tool, tool_handler, tool_router,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::daemon::try_request;
use crate::mcp_audit::{McpAuditLogger, McpAuditSpan};
use crate::protocol::Request;
use crate::{InspectResult, QueryInput, QueryRequest, QueryResult, SearchHit, WorkspaceIndex};

const DEFAULT_SEARCH_LIMIT: usize = 5;
const MAX_SEARCH_LIMIT: usize = 50;
const MAX_RETURNED_SEARCH_HITS: usize = 20;
const MAX_QUERY_CHARS: usize = 4_096;
const MAX_PREVIEW_CHARS: usize = 1_000;
const MAX_TEXT_FALLBACK_CHARS: usize = 2_000;
const DEFAULT_SYMBOL_LIMIT: usize = 200;
const MAX_SYMBOL_LIMIT: usize = 1_000;
const DEFAULT_INSPECT_LINES: usize = 80;
const MAX_INSPECT_LINES: usize = 500;
const DEFAULT_INSPECT_CHARS: usize = 16 * 1024;
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
    /// Requested ranked results. Defaults to 5; values above 20 are accepted for
    /// compatibility but compacted to 20. Expand only when the first result set
    /// lacks the needed evidence.
    #[serde(default)]
    #[schemars(schema_with = "optional_integer_schema", range(min = 1, max = 50))]
    pub limit: Option<usize>,
    /// Optional search scopes. A registered root or an existing parent directory
    /// containing one or more registered roots is accepted.
    #[serde(default)]
    pub roots: Option<Vec<PathBuf>>,
    /// Optional file kinds. Use source for code; text for SQL/Markdown/logs;
    /// agent_instructions for AGENTS.md; agent_skill for SKILL.md;
    /// agent_memory for project-scoped cross-Agent memory;
    /// semi_structured for .json; tabular for .csv/.tsv/.jsonl/.ndjson/.parquet.
    /// Omit this filter when the file kind is uncertain.
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
    /// Optional absolute or root-relative path prefix.
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Optional workspace file or directory whose applicable AGENTS.md hierarchy
    /// and project-scoped Agent memory should be included and ranked.
    #[serde(default)]
    pub context_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceInspectRequest {
    /// Absolute or current-workspace-relative path already present in the index.
    pub path: PathBuf,
    /// Exact indexed symbol to center the excerpt around.
    #[serde(default)]
    pub symbol: Option<String>,
    /// Search identifier returned by workspace_search, used to link retrieval
    /// and inspection in the audit trail.
    #[serde(default)]
    pub search_id: Option<String>,
    /// Maximum number of symbols to return. Defaults to 200 and cannot exceed 1000.
    #[serde(default)]
    #[schemars(schema_with = "optional_integer_schema", range(min = 1, max = 1_000))]
    pub max_symbols: Option<usize>,
    /// First one-based source line to return. Defaults to 1.
    #[serde(default)]
    #[schemars(schema_with = "optional_integer_schema", range(min = 1))]
    pub start_line: Option<usize>,
    /// Maximum source lines to return. Defaults to 80 and cannot exceed 500.
    #[serde(default)]
    #[schemars(schema_with = "optional_integer_schema", range(min = 1, max = 500))]
    pub max_lines: Option<usize>,
    /// Maximum source characters to return. Defaults to 16384 and cannot exceed 65536.
    #[serde(default)]
    #[schemars(schema_with = "optional_integer_schema", range(min = 1, max = 65_536))]
    pub max_chars: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WorkspaceQueryInput {
    /// CSV, TSV, JSON, JSONL, NDJSON, or Parquet file beneath an allowed root.
    pub path: PathBuf,
    /// SQL relation name. Defaults to data_0, data_1, and so on.
    #[serde(default)]
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
    #[serde(default)]
    #[schemars(schema_with = "optional_integer_schema", range(min = 1, max = 1_000))]
    pub max_rows: Option<usize>,
    /// Maximum serialized result bytes. Defaults to 1 MiB and cannot exceed 2 MiB.
    #[serde(default)]
    #[schemars(
        schema_with = "optional_integer_schema",
        range(min = 1_024, max = 2_097_152)
    )]
    pub max_bytes: Option<usize>,
    /// Query timeout in milliseconds. Defaults to 10 seconds and cannot exceed 30 seconds.
    #[serde(default)]
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
    search_sequence: Arc<AtomicU64>,
    tool_router: ToolRouter<Self>,
}

impl AwiMcpServer {
    pub fn new(index_dir: PathBuf, socket_path: PathBuf) -> Self {
        Self {
            index_dir,
            socket_path,
            audit_logger: None,
            search_sequence: Arc::new(AtomicU64::new(0)),
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
            search_sequence: Arc::new(AtomicU64::new(0)),
            tool_router: Self::tool_router(),
        })
    }

    fn audit_span(&self, tool: &'static str, arguments: Value) -> Option<McpAuditSpan> {
        self.audit_logger
            .as_ref()
            .map(|logger| logger.span(tool, arguments))
    }

    fn next_search_id(&self) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let sequence = self.search_sequence.fetch_add(1, Ordering::Relaxed) + 1;
        format!("s-{now:x}-{:x}-{sequence:x}", std::process::id())
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
    /// Preferred discovery step for code, symbols, docs, Agent instructions,
    /// Skills, memory, or dataset schemas in an indexed workspace. Start with one
    /// identifier-rich query and limit 5; do not issue parallel near-synonym
    /// searches. Inspect the best hit, then refine once only if evidence is
    /// missing. For prior decisions/history, set kinds=["agent_memory"] and pass
    /// context_path. If the exact path is already known, use a file reader directly.
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
        let search_id = self.next_search_id();
        let audit = self.audit_span(
            "workspace_search",
            json!({
                "query": &arguments.query,
                "limit": arguments.limit,
                "roots": &arguments.roots,
                "kinds": &arguments.kinds,
                "path_prefix": &arguments.path_prefix,
                "context_path": &arguments.context_path,
                "search_id": &search_id
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

        let requested_limit = arguments.limit.unwrap_or(DEFAULT_SEARCH_LIMIT);
        if !(1..=MAX_SEARCH_LIMIT).contains(&requested_limit) {
            return audited(
                audit,
                Err(McpError::invalid_params(
                    format!("limit must be between 1 and {MAX_SEARCH_LIMIT}"),
                    None,
                )),
            );
        }
        let limit = effective_search_limit(requested_limit);

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
        let hits = hits.into_iter().map(compact_search_hit).collect::<Vec<_>>();
        let returned = hits.len();

        audited(
            audit,
            Ok(bounded_result(json!({
                "search_id": search_id,
                "hits": hits,
                "returned": returned,
                "requested_limit": requested_limit,
                "effective_limit": limit,
                "limit_compacted": requested_limit > limit,
                "previews_truncated": previews_truncated,
                "format": "compact_v3"
            }))),
        )
    }

    /// Inspect one path returned by workspace_search. Returns metadata, symbols,
    /// schema, and a bounded line-numbered excerpt. If a path was not returned by
    /// search, use ordinary file tools instead of probing unindexed paths.
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
                "symbol": &arguments.symbol,
                "parent_search_id": &arguments.search_id,
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
                symbol: arguments.symbol,
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
                "focused_symbol": result.focused_symbol,
                "dataset": result.dataset,
                "agent": result.agent,
                "memory": result.memory,
                "content": result.content,
                "parent_search_id": arguments.search_id,
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
    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(ProtocolVersion::known_up_to(&ProtocolVersion::V_2025_11_25))
    }

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
                 walking. For prior decisions, history, or cross-Agent memory, set \
                 kinds=[\"agent_memory\"] and pass context_path. Pass context_path when resolving \
                 applicable AGENTS.md instructions. Start with one identifier-rich query and \
                 limit 5; do not issue parallel near-synonym searches. Inspect the best hit, \
                 then refine once only if the first result set lacks evidence. \
                 Non-empty previews are direct excerpts from the indexed generation and are \
                 sufficient evidence when they contain the required facts. Once an authoritative \
                 path is selected, do not repeat discovery searches. Use one workspace_inspect \
                 call, with up to 500 lines for long text, only for missing details; use \
                 workspace_query directly for structured aggregation. If a path is not in an \
                 indexed root, fall back to ordinary file tools instead of passing it here. When \
                 the user already supplied an exact path, read that path directly.",
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
            symbol,
            start_line,
            max_lines,
            max_chars,
        } => serde_json::to_value(match symbol {
            Some(symbol) => workspace.inspect_symbol(path, &symbol, max_lines, max_chars)?,
            None => workspace.inspect_excerpt(path, start_line, max_lines, max_chars)?,
        })
        .map_err(Into::into),
        Request::Query { request } => {
            serde_json::to_value(workspace.query(&request)?).map_err(Into::into)
        }
        _ => anyhow::bail!("unsupported MCP workspace request"),
    }
}

fn tool_error(code: &str, error: &anyhow::Error) -> CallToolResult {
    let recovery = match code {
        "search_failed" => Some(
            "Omit roots or use a registered root/parent scope; use a short query and retry once.",
        ),
        "inspect_failed" => Some(
            "Call workspace_search first and inspect only a returned path; otherwise use ordinary file tools.",
        ),
        "query_failed" => Some(
            "Use an exact registered root and explicit structured input files with one read-only SELECT/WITH statement.",
        ),
        _ => None,
    };
    structured_result(
        json!({
            "error": {
                "code": code,
                "message": format!("{error:#}"),
                "recovery": recovery
            }
        }),
        true,
    )
}

fn compact_search_hit(hit: SearchHit) -> Value {
    let mut value = json!({
        "file_id": hit.file_id,
        "path": hit.path,
        "kind": hit.kind,
        "score": hit.score,
        "matched_lanes": hit.matched_lanes,
        "preview": hit.preview,
        "generation": hit.generation
    });
    let object = value.as_object_mut().expect("search hit is an object");
    if let Some(experiment) = hit.experiment {
        object.insert("experiment".to_owned(), Value::String(experiment));
    }
    if let Some(symbol) = hit.symbol {
        object.insert("symbol".to_owned(), json!(symbol));
    }
    if let Some(agent) = hit.agent {
        object.insert(
            "agent".to_owned(),
            json!({
                "role": agent.role,
                "name": agent.name,
                "description": agent.description,
                "scope_root": agent.scope_root
            }),
        );
    }
    if let Some(memory) = hit.memory {
        object.insert("memory".to_owned(), json!(memory));
    }
    value
}

fn effective_search_limit(requested: usize) -> usize {
    requested.min(MAX_RETURNED_SEARCH_HITS)
}

fn bounded_result(value: Value) -> CallToolResult {
    match serde_json::to_vec(&value) {
        Ok(encoded) if encoded.len() <= MAX_STRUCTURED_RESPONSE_BYTES => {
            structured_result(value, false)
        }
        Ok(encoded) => structured_result(
            json!({
                "error": {
                    "code": "response_too_large",
                    "message": format!(
                        "response is {} bytes; maximum is {MAX_STRUCTURED_RESPONSE_BYTES} bytes",
                        encoded.len()
                    )
                }
            }),
            true,
        ),
        Err(error) => tool_error("response_serialization_failed", &error.into()),
    }
}

fn structured_result(value: Value, is_error: bool) -> CallToolResult {
    let content = vec![ContentBlock::text(text_fallback(&value))];
    let mut result = if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    };
    result.structured_content = Some(value);
    result
}

fn text_fallback(value: &Value) -> String {
    let mut summary = if let Some(error) = value.get("error") {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("AWI tool error");
        let recovery = error
            .get("recovery")
            .and_then(Value::as_str)
            .map(|value| format!("\nRecovery: {value}"))
            .unwrap_or_default();
        format!("AWI error [{code}]: {message}{recovery}")
    } else if let Some(hits) = value.get("hits").and_then(Value::as_array) {
        let paths = hits
            .iter()
            .take(5)
            .filter_map(|hit| hit.get("path").and_then(Value::as_str))
            .collect::<Vec<_>>();
        if paths.is_empty() {
            "AWI search returned no hits.".to_owned()
        } else {
            format!(
                "AWI search returned {} hit(s). Top paths:\n{}",
                hits.len(),
                paths.join("\n")
            )
        }
    } else if let Some(file) = value.get("file") {
        let path = file
            .get("absolute_path")
            .and_then(Value::as_str)
            .unwrap_or("unknown path");
        let symbol_count = value
            .get("symbols")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        let excerpt = value
            .pointer("/content/text")
            .and_then(Value::as_str)
            .map(|text| format!("\nExcerpt:\n{text}"))
            .unwrap_or_default();
        format!("AWI inspect: {path} ({symbol_count} symbol(s)){excerpt}")
    } else if let Some(row_count) = value.get("row_count").and_then(Value::as_u64) {
        let columns = value.get("columns").cloned().unwrap_or(Value::Null);
        let rows = value
            .get("rows")
            .and_then(Value::as_array)
            .map(|rows| rows.iter().take(3).cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        format!(
            "AWI query returned {row_count} row(s). Columns: {columns}. First rows: {}",
            Value::Array(rows)
        )
    } else {
        "AWI result is available in structuredContent.".to_owned()
    };
    truncate_chars(&mut summary, MAX_TEXT_FALLBACK_CHARS);
    summary
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
    fn structured_result_uses_a_bounded_text_fallback_without_duplicating_json() {
        let payload = "x".repeat(16_000);
        let result = bounded_result(json!({
            "payload": payload
        }));
        let text = result.content[0].as_text().unwrap().text.as_str();
        assert_eq!(text, "AWI result is available in structuredContent.");
        assert_eq!(
            result.structured_content.unwrap()["payload"]
                .as_str()
                .unwrap()
                .len(),
            16_000
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

    #[test]
    fn compacts_broad_search_limits_without_rejecting_compatible_requests() {
        assert_eq!(effective_search_limit(5), 5);
        assert_eq!(effective_search_limit(20), 20);
        assert_eq!(effective_search_limit(50), 20);
    }

    #[test]
    fn optional_tool_parameters_are_not_required_by_the_schema() {
        let schema = serde_json::to_value(schemars::schema_for!(WorkspaceSearchRequest)).unwrap();
        assert_eq!(schema["required"], json!(["query"]));
    }
}
