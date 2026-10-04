use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, ContentBlock, Implementation, InitializeRequestParams, InitializeResult,
        ProtocolVersion, ServerCapabilities, ServerConfig,
    },
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
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
// Some clients deliver only content[].text to the model. This is an evidence
// channel, not a pointer to structuredContent. Keep its own explicit bound.
const MAX_TEXT_FALLBACK_CHARS: usize = 96 * 1024;
const DEFAULT_SYMBOL_LIMIT: usize = 24;
const DEFAULT_PREVIEW_BUDGET: usize = 3_000;
const MAX_PREVIEW_BUDGET: usize = 20_000;
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
    /// Optional search scopes. Accepts registered roots, their existing child
    /// directories, or an existing parent containing registered roots.
    #[serde(default)]
    pub roots: Option<Vec<PathBuf>>,
    /// Optional file kinds. Use source for code; text for SQL/Markdown/logs;
    /// agent_instructions for AGENTS.md; agent_skill for SKILL.md;
    /// agent_memory for project-scoped cross-Agent memory;
    /// semi_structured for .json; tabular for .csv/.tsv/.jsonl/.ndjson/.parquet.
    /// Omit this filter to search every indexed kind, including Agent knowledge
    /// and memory. Set it only when the request should be narrowed.
    #[serde(default)]
    pub kinds: Option<Vec<String>>,
    /// Optional absolute or root-relative path prefix.
    #[serde(default)]
    pub path_prefix: Option<String>,
    /// Optional workspace file or directory whose applicable AGENTS.md hierarchy
    /// and project-scoped Agent memory should be included and ranked.
    #[serde(default)]
    pub context_path: Option<PathBuf>,
    /// Total preview character budget across all hits (500..20000; default 3000).
    /// Higher-ranked hits receive more evidence; inspect for longer excerpts.
    #[serde(default)]
    #[schemars(
        schema_with = "optional_integer_schema",
        range(min = 500, max = 20_000)
    )]
    pub preview_chars: Option<usize>,
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
    /// Maximum symbols in the outline. Defaults to 24 (definitions near the excerpt first); maximum 1000.
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

#[derive(Debug, Default)]
struct ClientDelivery {
    structured_only: bool,
    reader_at_initialize: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct AwiMcpServer {
    index_dir: PathBuf,
    socket_path: PathBuf,
    audit_logger: Option<Arc<McpAuditLogger>>,
    search_sequence: Arc<AtomicU64>,
    tool_router: ToolRouter<Self>,
    delivery: Arc<RwLock<ClientDelivery>>,
}

impl AwiMcpServer {
    pub fn new(index_dir: PathBuf, socket_path: PathBuf) -> Self {
        Self {
            index_dir,
            socket_path,
            audit_logger: None,
            search_sequence: Arc::new(AtomicU64::new(0)),
            tool_router: Self::tool_router(),
            delivery: Arc::new(RwLock::new(ClientDelivery::default())),
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
            delivery: Arc::new(RwLock::new(ClientDelivery::default())),
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

    async fn execute(&self, request: Request) -> Result<DaemonReply> {
        let index_dir = self.index_dir.clone();
        let socket_path = self.socket_path.clone();
        tokio::task::spawn_blocking(move || execute_request(&index_dir, &socket_path, request))
            .await
            .context("join AWI MCP request worker")?
    }
}

/// Daemon reply: the payload value plus optional per-request stage diagnostics
/// carried in the response envelope.
#[derive(Debug, Default, Clone)]
pub struct DaemonReply {
    pub value: Value,
    pub stages: Option<Value>,
}

#[tool_router]
impl AwiMcpServer {
    /// Preferred discovery step for code, symbols, docs, Agent instructions,
    /// Skills, memory, or dataset schemas in an indexed workspace. Start with one
    /// identifier-rich query and limit 5; do not issue parallel near-synonym
    /// searches. Inspect the best hit, then refine once only if evidence is
    /// missing. Omit kinds to search every indexed kind; set kinds only to narrow
    /// the corpus. Pass context_path for project-scoped memory and applicable
    /// instructions. If the exact path is already known, use a file reader directly.
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
                "search_id": &search_id,
                "preview_chars": arguments.preview_chars
            }),
        );
        let query = arguments.query.trim();
        if query.is_empty() {
            return audited(
                self,
                audit,
                Err(McpError::invalid_params("query must not be empty", None)),
            );
        }
        if query.chars().count() > MAX_QUERY_CHARS {
            return audited(
                self,
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
                self,
                audit,
                Err(McpError::invalid_params(
                    format!("limit must be between 1 and {MAX_SEARCH_LIMIT}"),
                    None,
                )),
            );
        }
        let limit = effective_search_limit(requested_limit);

        let preview_budget = arguments.preview_chars.unwrap_or(DEFAULT_PREVIEW_BUDGET);
        if !(500..=MAX_PREVIEW_BUDGET).contains(&preview_budget) {
            return audited(
                self,
                audit,
                Err(McpError::invalid_params(
                    "preview_chars must be between 500 and 20000",
                    None,
                )),
            );
        }
        let diagnostic_request = Request::DiagnoseSearch {
            anchors: crate::search::filename_anchors(query),
            roots: arguments.roots.clone().unwrap_or_default(),
            context_path: arguments.context_path.clone(),
        };
        let req_path_prefix = arguments.path_prefix.clone();
        let req_roots = arguments.roots.clone().unwrap_or_default();
        let reply = self
            .execute(Request::Search {
                query: query.to_owned(),
                limit,
                roots: arguments.roots.unwrap_or_default(),
                kinds: arguments.kinds.unwrap_or_default(),
                path_prefix: arguments.path_prefix,
                context_path: arguments.context_path,
            })
            .await;
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                let diagnostics = self
                    .execute(diagnostic_request)
                    .await
                    .ok()
                    .map(|reply| reply.value);
                let mut result = tool_error("search_failed", &error);
                if let Some(mut value) = result.structured_content.take() {
                    value["diagnostics"] = json!(diagnostics);
                    result = structured_result(value, true);
                }
                return audited(self, audit, Ok(result));
            }
        };
        let value = reply.value;
        let daemon_stages = reply.stages;
        let mut hits: Vec<SearchHit> = match serde_json::from_value(value) {
            Ok(hits) => hits,
            Err(error) => {
                return audited(
                    self,
                    audit,
                    Ok(tool_error("invalid_search_response", &error.into())),
                );
            }
        };
        let previews_truncated = budget_previews(&mut hits, query, preview_budget);
        let unmatched_filenames = crate::search::filename_anchors(query)
            .into_iter()
            .filter(|anchor| {
                !hits.iter().any(|hit| {
                    hit.path == *anchor
                        || (!anchor.starts_with('/') && hit.path.ends_with(&format!("/{anchor}")))
                })
            })
            .collect::<Vec<_>>();
        let diagnostics = if hits.is_empty() || !unmatched_filenames.is_empty() {
            self.execute(diagnostic_request)
                .await
                .map(|reply| reply.value)
                .unwrap_or_else(|_| json!({
                "scope":"unavailable", "workspace_completeness":"unknown", "items":[{
                    "reason":"diagnostics_unavailable", "next_action":"Check reader version and scope, or use file tools."
                }]
            }))
        } else {
            Value::Null
        };
        let preview_chars_used: usize = hits.iter().map(|hit| hit.preview.chars().count()).sum();
        let hits = hits.into_iter().map(compact_search_hit).collect::<Vec<_>>();
        let returned = hits.len();

        let anchor_total = crate::search::filename_anchors(query).len();
        let literal_matched = anchor_total.saturating_sub(unmatched_filenames.len());
        let literal_match = json!({
            "anchors_total": anchor_total,
            "anchors_matched": literal_matched,
            "anchors_unmatched": unmatched_filenames.len(),
            "any_literal_hit": anchor_total > 0 && unmatched_filenames.is_empty(),
        });
        // scope_coverage derived from the PARSED REQUEST parameters, not from a
        // missing diagnostics probe: path_prefix => path_prefix scope; explicit
        // roots => roots scope with requested/total counts; otherwise workspace.
        // We never fabricate coverage: a path_prefix request is applied when the
        // parameter was accepted into the search; roots coverage reports the
        // number of requested roots.
        let scope_coverage = if let Some(prefix) = req_path_prefix.as_deref().filter(|p| !p.is_empty()) {
            json!({
                "scope": "path_prefix",
                "requested": {"path_prefix": prefix},
                "effective": {"path_prefix": prefix},
                "covered": true,
                "reason": "path_prefix parameter accepted and applied to search",
            })
        } else if !req_roots.is_empty() {
            let n = req_roots.len();
            json!({
                "scope": "roots",
                "requested": {"roots": n},
                "effective": {"roots": n},
                "covered_roots": n,
                "total_roots": n,
                "coverage_ratio": 1.0,
                "reason": "requested roots passed through to search; accepted count equals requested",
            })
        } else {
            json!({
                "scope": "workspace",
                "covered": true,
                "reason": "no narrowing scope requested; search over whole indexed workspace",
            })
        };

        let result = bounded_result(json!({
            "search_id": search_id,
            "hits": hits,
            "returned": returned,
            "unmatched_filenames": unmatched_filenames,
            "literal_match": literal_match,
            "scope_coverage": scope_coverage,
            "requested_limit": requested_limit,
            "effective_limit": limit,
            "limit_compacted": requested_limit > limit,
            "previews_truncated": previews_truncated,
            "preview_budget": {"max_chars":preview_budget,"used_chars":preview_chars_used},
            "diagnostics": diagnostics,
            "format": "compact_v4"
        }));
        // The final-response serialization time is measured inside
        // audited_with_stages AFTER delivery.runtime.stages is injected, so it
        // covers the actual on-wire envelope, not just the search body.
        audited_with_stages(self, audit, Ok(result), daemon_stages)
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
                self,
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
                self,
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
                self,
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
                self,
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
            Ok(reply) => reply.value,
            Err(error) => {
                return audited(self, audit, Ok(tool_error("inspect_failed", &error)));
            }
        };
        let mut result: InspectResult = match serde_json::from_value(value) {
            Ok(result) => result,
            Err(error) => {
                return audited(
                    self,
                    audit,
                    Ok(tool_error("invalid_inspect_response", &error.into())),
                );
            }
        };
        let total_symbols = result.symbols.len();
        if arguments.max_symbols.is_none() {
            let line = result
                .content
                .as_ref()
                .map_or(start_line, |content| content.start_line);
            result.symbols.sort_by_key(|symbol| {
                (
                    matches!(symbol.kind.as_str(), "call" | "import"),
                    symbol.line_start.abs_diff(line),
                    symbol.line_start,
                )
            });
        }
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
            self,
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
                self,
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
                self,
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
                self,
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
            Ok(reply) => reply.value,
            Err(error) => {
                return audited(self, audit, Ok(tool_error("query_failed", &error)));
            }
        };
        let result: QueryResult = match serde_json::from_value(value) {
            Ok(result) => result,
            Err(error) => {
                return audited(
                    self,
                    audit,
                    Ok(tool_error("invalid_query_response", &error.into())),
                );
            }
        };
        audited(self, audit, Ok(bounded_result(json!(result))))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AwiMcpServer {
    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        if let Some(logger) = &self.audit_logger {
            logger.observe_client_name(&request.client_info.name);
        }
        let request_json = serde_json::to_value(&request).unwrap_or(Value::Null);
        let structured_only = request_json
            .pointer("/capabilities/experimental/awi/response_channel")
            .and_then(Value::as_str)
            == Some("structured");
        let reader_at_initialize = self
            .execute(Request::Ping)
            .await
            .ok()
            .map(|reply| reply.value);
        *self
            .delivery
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = ClientDelivery {
            structured_only,
            reader_at_initialize,
        };
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

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
                 walking. Omit kinds to search all indexed code, data, Agent knowledge, \
                 and memory; set kinds only to narrow the corpus. Pass context_path for \
                 project-scoped memory and applicable AGENTS.md instructions. Start with one identifier-rich query and \
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
    server: &AwiMcpServer,
    audit: Option<McpAuditSpan>,
    result: Result<CallToolResult, McpError>,
) -> Result<CallToolResult, McpError> {
    audited_with_stages(server, audit, result, None)
}

fn audited_with_stages(
    server: &AwiMcpServer,
    audit: Option<McpAuditSpan>,
    mut result: Result<CallToolResult, McpError>,
    stages: Option<Value>,
) -> Result<CallToolResult, McpError> {
    if let Ok(response) = &mut result {
        let delivery = server
            .delivery
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut runtime = json!({
            "adapter_version":env!("CARGO_PKG_VERSION"), "adapter_build_id":env!("AWI_BUILD_ID"),
            "adapter_pid":std::process::id(), "adapter_executable_replaced":executable_replaced(),
            "reader_at_initialize":delivery.reader_at_initialize,
            "channel":if delivery.structured_only { "structured" } else { "text_and_structured" },
            "client_receipt":"unverified"
        });
        // Per-query five-stage bundle (queue wait / reader snapshot open / sidecar
        // sub-stages / final response serialization). The audit logger already
        // extracts /delivery/runtime, so this lands in mcp-calls.jsonl.
        if let Some(stages) = stages {
            runtime["stages"] = stages;
        }
        if let Some(value) = &mut response.structured_content {
            value["delivery"]["runtime"] = runtime.clone();
            if delivery.structured_only {
                response.content = vec![ContentBlock::text(format!(
                    "AWI result in negotiated structuredContent; build={}",
                    env!("AWI_BUILD_ID")
                ))];
                value["delivery"]["text_chars"] =
                    json!(response.content[0].as_text().unwrap().text.chars().count());
                value["delivery"]["text_truncated"] = json!(false);
            } else {
                let reader_build = runtime
                    .pointer("/reader_at_initialize/build_id")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let mut marker = format!(
                    "AWI delivery: adapter={} reader_at_initialize={} channel=both client_receipt=unverified",
                    env!("AWI_BUILD_ID"),
                    reader_build
                );
                if runtime["adapter_executable_replaced"] == true {
                    marker.push_str("; adapter replaced: reconnect this MCP client");
                }
                response.content.push(ContentBlock::text(marker));
                value["delivery"]["text_chars"] = json!(
                    response
                        .content
                        .iter()
                        .filter_map(|block| block.as_text())
                        .map(|block| block.text.chars().count())
                        .sum::<usize>()
                );
            }
        }
        // NOTE: The final rmcp stdio JSONRPC serialization is owned by the rmcp
        // transport and cannot be hooked here without forking it. The real
        // on-wire serialization cost is measured in the daemon at the actual
        // `serde_json::to_writer` to the socket and surfaced as
        // `daemon_response_serialization_ms` inside delivery.runtime.stages.
        // We do NOT run a separate probe `to_vec` here and discard it, since that
        // would be a simulation rather than the transport write.
    }
    if let Some(audit) = audit {
        audit.finish(&result);
    }
    result
}

fn executable_replaced() -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let running = std::fs::metadata("/proc/self/exe").ok()?;
    let path = std::fs::read_link("/proc/self/exe").ok()?;
    let text = path.to_string_lossy();
    let installed = Path::new(text.strip_suffix(" (deleted)").unwrap_or(&text));
    Some(std::fs::metadata(installed).map_or(true, |meta| {
        meta.ino() != running.ino() || meta.dev() != running.dev()
    }))
}

fn execute_request(index_dir: &Path, socket_path: &Path, request: Request) -> Result<DaemonReply> {
    if let Some((value, stages)) = try_request(socket_path, &request)? {
        return Ok(DaemonReply { value, stages });
    }

    if matches!(request, Request::Ping) {
        return Ok(DaemonReply {
            value: json!({"service":"awi","mode":"direct_index","version":env!("CARGO_PKG_VERSION"),
            "protocol_version":crate::protocol::PROTOCOL_VERSION,"build_id":env!("AWI_BUILD_ID")}),
            stages: None,
        });
    }
    let workspace = WorkspaceIndex::open(index_dir)
        .with_context(|| format!("open AWI index {}", index_dir.display()))?;
    let value = match request {
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
        Request::DiagnoseSearch {
            anchors,
            roots,
            context_path,
        } => workspace.diagnose_search(&anchors, &roots, context_path.as_deref()),
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
    }?;
    // Direct (no-daemon) mode has no worker queue and no live sidecar process;
    // every unmeasurable stage is recorded as null with an explicit reason rather
    // than a fabricated number, so audit consumers can tell "not measured here"
    // from "measured but zero".
    let semantic_disabled = std::env::var_os("AWI_SEMANTIC_MODEL").is_none();
    Ok(DaemonReply {
        value,
        stages: Some(json!({
            "retrieval_mode": "direct",
            "queue_wait_ms": 0.0,
            "reader_snapshot_open_ms": Value::Null,
            "reader_snapshot_reopened": false,
            "sidecar_startup_ms": Value::Null,
            "sidecar_startup_state": "disabled",
            "sidecar_startup_reason": if semantic_disabled { "semantic_model_unset" } else { "direct_inprocess_no_sidecar" },
            "embedding_ms": Value::Null,
            "semantic_reason": if semantic_disabled { "AWI_SEMANTIC_MODEL_unset" } else { "not_served_by_sidecar_in_direct_mode" }
        })),
    })
}

fn tool_error(code: &str, error: &anyhow::Error) -> CallToolResult {
    let recovery = match code {
        "search_failed" if format!("{error:#}").contains("context path") => Some(
            "Pass an existing indexed task directory as context_path, or omit it. Check diagnostics before retrying.",
        ),
        "search_failed" => Some(
            "Check diagnostic reasons and query filters; use file tools when the target is outside indexed coverage.",
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
    if let Some(context) = hit.source_context {
        object.insert("source_context".to_owned(), json!(context));
    }
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

fn budget_previews(hits: &mut [SearchHit], query: &str, budget: usize) -> bool {
    let count = hits.len();
    let floor = 128.min(budget / count.max(1));
    let mut remaining = budget;
    let mut truncated = false;
    for (rank, hit) in hits.iter_mut().enumerate() {
        let allowance = remaining
            .saturating_sub((count - rank - 1) * floor)
            .min(MAX_PREVIEW_CHARS);
        if hit.preview.chars().count() > allowance {
            // Preserve a query-bearing window instead of blindly taking the head.
            let match_byte = query
                .split(|ch: char| !ch.is_alphanumeric() && ch != '_')
                .filter(|term| term.len() > 2)
                .filter_map(|term| hit.preview.find(term))
                .min()
                .unwrap_or(0);
            let match_char = hit.preview[..match_byte].chars().count();
            let offset = match_char.saturating_sub(allowance / 3);
            hit.preview = hit.preview.chars().skip(offset).take(allowance).collect();
            truncated = true;
        }
        remaining = remaining.saturating_sub(hit.preview.chars().count());
    }
    truncated
}

fn effective_search_limit(requested: usize) -> usize {
    requested.min(MAX_RETURNED_SEARCH_HITS)
}

fn bounded_result(value: Value) -> CallToolResult {
    match serde_json::to_vec(&value) {
        Ok(encoded) if encoded.len().saturating_add(1_024) <= MAX_STRUCTURED_RESPONSE_BYTES => {
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

fn structured_result(mut value: Value, is_error: bool) -> CallToolResult {
    let mut text = text_fallback(&value);
    if let Some(diagnostics) = value.get("diagnostics").filter(|value| !value.is_null()) {
        text.push_str(&format!("\nDiagnostics: {diagnostics}\n"));
    }
    let text_truncated = truncate_chars(&mut text, MAX_TEXT_FALLBACK_CHARS - 512);
    if text_truncated {
        text.push_str(
            "\n[TEXT TRUNCATED: remaining evidence is omitted. Narrow the search, \
             paginate inspect with start_line/max_lines, or reduce query rows/columns.]",
        );
    }
    value["delivery"] = json!({
        "text_truncated": text_truncated,
        "text_chars": text.chars().count(),
        "max_text_chars": MAX_TEXT_FALLBACK_CHARS
    });
    let content = vec![ContentBlock::text(text)];
    let mut result = if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    };
    result.structured_content = Some(value);
    result
}

fn text_fallback(value: &Value) -> String {
    if let Some(error) = value.get("error") {
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
        let mut text = format!(
            "AWI search_id={} returned={} limit_compacted={} previews_truncated={}\n\
             Previews are excerpts of indexed evidence; generation is per file/shard.\n",
            value["search_id"],
            hits.len(),
            value["limit_compacted"],
            value["previews_truncated"]
        );
        if let Some(unmatched) = value.get("unmatched_filenames").and_then(Value::as_array)
            && !unmatched.is_empty()
        {
            text.push_str(&format!(
                "No literal filename/path match in returned hits for {}. Related results follow; \
                 check roots/ignore rules or use file tools before another synonym search.\n",
                Value::Array(unmatched.clone())
            ));
        }
        if hits.is_empty() {
            text.push_str("No hits. Refine once or use file tools for unindexed paths.\n");
        }
        for (rank, hit) in hits.iter().enumerate() {
            text.push_str(&format!(
                "\n{}. {} [{}; generation={}]\n",
                rank + 1,
                hit["path"].as_str().unwrap_or("unknown path"),
                hit["kind"].as_str().unwrap_or("unknown"),
                hit["generation"]
            ));
            for key in ["symbol", "agent", "memory", "source_context"] {
                if let Some(metadata) = hit.get(key).filter(|v| !v.is_null()) {
                    text.push_str(&format!("{key}: {metadata}\n"));
                }
            }
            text.push_str(hit["preview"].as_str().unwrap_or("(no indexed preview)"));
            text.push('\n');
        }
        text
    } else if let Some(file) = value.get("file") {
        let path = file
            .get("absolute_path")
            .and_then(Value::as_str)
            .unwrap_or("unknown path");
        let mut text = format!(
            "AWI inspect: {path}\nfile: {file}\ncoverage: {}\nparent_search_id: {}\n",
            value["coverage"], value["parent_search_id"]
        );
        if let Some(content) = value.get("content").filter(|v| !v.is_null()) {
            let mut bounds = content.clone();
            if let Some(object) = bounds.as_object_mut() {
                object.remove("text");
            }
            text.push_str(&format!(
                "excerpt: {bounds}\n{}\n",
                content["text"].as_str().unwrap_or("")
            ));
        }
        for key in ["focused_symbol", "dataset", "symbols", "agent", "memory"] {
            if let Some(metadata) = value.get(key).filter(|v| !v.is_null()) {
                text.push_str(&format!("{key}: {metadata}\n"));
            }
        }
        text
    } else if let Some(row_count) = value.get("row_count").and_then(Value::as_u64) {
        let columns = value.get("columns").cloned().unwrap_or(Value::Null);
        format!(
            "AWI query returned {row_count} row(s); result_truncated={}.\nColumns: {columns}\nRows: {}",
            value["truncated"], value["rows"]
        )
    } else {
        "AWI result is available in structuredContent.".to_owned()
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
    fn text_only_clients_receive_all_search_hits_inspect_evidence_and_query_rows() {
        let hits = (0..8)
            .map(|i| {
                json!({
                    "path": format!("/project/file{i}.rs"),
                    "kind": "source", "generation": 7,
                    "preview": format!("evidence_for_{i}"),
                    "symbol": {"name": format!("target_{i}"), "line_start": 9}
                })
            })
            .collect::<Vec<_>>();
        let search = bounded_result(json!({"search_id": "s-visible", "hits": hits}));
        let text = &search.content[0].as_text().unwrap().text;
        assert!(text.contains("s-visible"));
        assert!(text.contains("evidence_for_7"));
        assert!(text.contains("target_7"));
        assert!(text.contains("generation=7"));

        let excerpt = format!("{}END_OF_REQUESTED_EXCERPT", "公式\n".repeat(3000));
        let inspect = bounded_result(json!({
            "file": {"absolute_path": "/project/notes.md", "generation": 7},
            "content": {"text": excerpt, "start_line": 1, "end_line": 3001, "truncated": false},
            "dataset": {"columns": [{"name": "score", "data_type": "DOUBLE"}]}
        }));
        let text = &inspect.content[0].as_text().unwrap().text;
        assert!(text.contains("END_OF_REQUESTED_EXCERPT"));
        assert!(text.contains("DOUBLE"));
        assert_eq!(
            inspect.structured_content.unwrap()["delivery"]["text_truncated"],
            false
        );

        let query = bounded_result(json!({
            "row_count": 5, "columns": ["n"], "rows": [[1], [2], [3], [4], [5]], "truncated": true
        }));
        let text = &query.content[0].as_text().unwrap().text;
        assert!(text.contains("[5]"));
        assert!(text.contains("result_truncated=true"));
    }

    #[test]
    fn missing_filename_hint_does_not_claim_the_whole_corpus_was_absent() {
        let result = bounded_result(json!({
            "hits": [], "unmatched_filenames": ["ignored.py"]
        }));
        let text = &result.content[0].as_text().unwrap().text;
        assert!(text.contains("No literal filename/path match in returned hits"));
        assert!(text.contains("roots/ignore rules"));
        assert!(text.contains("ignored.py"));
    }

    #[test]
    fn oversized_text_delivery_is_explicit_and_utf8_bounded() {
        let result = bounded_result(json!({
            "row_count": 1, "columns": ["text"],
            "rows": [["公式".repeat(MAX_TEXT_FALLBACK_CHARS)]], "truncated": false
        }));
        let server = AwiMcpServer::new(PathBuf::from("unused"), PathBuf::from("unused.sock"));
        server.delivery.write().unwrap().reader_at_initialize = Some(json!({
            "build_id": "0123456789abcdef"
        }));
        let result = audited(&server, None, Ok(result)).unwrap();
        let text = result
            .content
            .iter()
            .filter_map(|block| block.as_text())
            .map(|block| block.text.as_str())
            .collect::<String>();
        assert!(text.chars().count() <= MAX_TEXT_FALLBACK_CHARS);
        assert!(text.contains("[TEXT TRUNCATED:"));
        assert_eq!(
            result.structured_content.unwrap()["delivery"]["text_truncated"],
            true
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
