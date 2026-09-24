use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use fs4::fs_std::FileExt;
use regex::Regex;
use rmcp::{ErrorData as McpError, model::CallToolResult};
use serde::Serialize;
use serde_json::{Map, Value, json};

const AUDIT_SCHEMA_VERSION: u8 = 4;
const DEFAULT_MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 16 * 1024;
const MAX_STRING_CHARS: usize = 2_048;
const MAX_ARRAY_ITEMS: usize = 16;
const MAX_OBJECT_FIELDS: usize = 32;
const MAX_VALUE_DEPTH: usize = 5;

#[derive(Debug)]
pub(crate) struct McpAuditLogger {
    path: PathBuf,
    lock_path: PathBuf,
    sequence: AtomicU64,
    max_log_bytes: u64,
    client_process: Option<String>,
}

impl McpAuditLogger {
    pub(crate) fn open(path: &Path) -> Result<Arc<Self>> {
        Self::open_with_limit(path, DEFAULT_MAX_LOG_BYTES)
    }

    fn open_with_limit(path: &Path, max_log_bytes: u64) -> Result<Arc<Self>> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)
                .with_context(|| format!("create AWI MCP audit directory {}", parent.display()))?;
        }
        let lock_path = sibling_path(path, ".lock");
        secure_append_file(path)?;
        secure_append_file(&lock_path)?;
        Ok(Arc::new(Self {
            path: path.to_owned(),
            lock_path,
            sequence: AtomicU64::new(0),
            max_log_bytes,
            client_process: parent_process_name(),
        }))
    }

    pub(crate) fn span(self: &Arc<Self>, tool: &'static str, arguments: Value) -> McpAuditSpan {
        let arguments = sanitize_value(arguments, 0);
        let encoded_arguments = serde_json::to_vec(&arguments).unwrap_or_default();
        McpAuditSpan {
            logger: Arc::clone(self),
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
            started_at_unix_ms: unix_time_ms(),
            started: Instant::now(),
            tool,
            arguments,
            arguments_blake3: blake3::hash(&encoded_arguments).to_hex().to_string(),
        }
    }

    fn write(&self, record: &AuditRecord<'_>) -> Result<()> {
        let lock_file = secure_append_file(&self.lock_path)?;
        lock_file
            .lock_exclusive()
            .with_context(|| format!("lock AWI MCP audit log {}", self.lock_path.display()))?;

        let write_result = (|| {
            let mut value =
                serde_json::to_value(record).context("serialize AWI MCP audit record")?;
            let mut encoded = serde_json::to_vec(&value).context("encode AWI MCP audit record")?;
            if encoded.len() > MAX_RECORD_BYTES {
                value["arguments"] = json!({
                    "omitted": "record_size_limit",
                    "original_bytes": encoded.len()
                });
                encoded =
                    serde_json::to_vec(&value).context("encode bounded AWI MCP audit record")?;
            }
            if encoded.len() > MAX_RECORD_BYTES {
                anyhow::bail!(
                    "AWI MCP audit record is {} bytes; maximum is {MAX_RECORD_BYTES}",
                    encoded.len()
                );
            }
            encoded.push(b'\n');

            self.rotate_if_needed(encoded.len() as u64)?;
            let mut file = secure_append_file(&self.path)?;
            file.write_all(&encoded)
                .with_context(|| format!("append AWI MCP audit log {}", self.path.display()))?;
            file.flush()
                .with_context(|| format!("flush AWI MCP audit log {}", self.path.display()))
        })();

        let unlock_result = FileExt::unlock(&lock_file)
            .with_context(|| format!("unlock AWI MCP audit log {}", self.lock_path.display()));
        write_result?;
        unlock_result
    }

    fn rotate_if_needed(&self, incoming_bytes: u64) -> Result<()> {
        let current_bytes = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("stat AWI MCP audit log {}", self.path.display()));
            }
        };
        if current_bytes == 0 || current_bytes.saturating_add(incoming_bytes) <= self.max_log_bytes
        {
            return Ok(());
        }

        let rotated = sibling_path(&self.path, ".1");
        match fs::remove_file(&rotated) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("remove rotated audit log {}", rotated.display()));
            }
        }
        fs::rename(&self.path, &rotated).with_context(|| {
            format!(
                "rotate AWI MCP audit log {} to {}",
                self.path.display(),
                rotated.display()
            )
        })?;
        fs::set_permissions(&rotated, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("secure rotated audit log {}", rotated.display()))?;
        secure_append_file(&self.path)?;
        Ok(())
    }
}

pub(crate) struct McpAuditSpan {
    logger: Arc<McpAuditLogger>,
    sequence: u64,
    started_at_unix_ms: u64,
    started: Instant,
    tool: &'static str,
    arguments: Value,
    arguments_blake3: String,
}

impl McpAuditSpan {
    pub(crate) fn finish(self, result: &Result<CallToolResult, McpError>) {
        let metrics = response_metrics(self.tool, result);
        let parent_search_id = (self.tool == "workspace_inspect")
            .then(|| {
                self.arguments
                    .get("parent_search_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .flatten();
        let record = AuditRecord {
            schema_version: AUDIT_SCHEMA_VERSION,
            timestamp_unix_ms: self.started_at_unix_ms,
            pid: std::process::id(),
            client_process: self.logger.client_process.as_deref(),
            sequence: self.sequence,
            tool: self.tool,
            arguments: self.arguments,
            arguments_blake3: &self.arguments_blake3,
            duration_ms: self.started.elapsed().as_millis(),
            status: metrics.status,
            error_code: metrics.error_code,
            error_message: metrics.error_message,
            response_bytes: metrics.response_bytes,
            text_content_bytes: metrics.text_content_bytes,
            structured_content_bytes: metrics.structured_content_bytes,
            result_count: metrics.result_count,
            result_count_kind: metrics.result_count_kind,
            result_truncated: metrics.result_truncated,
            preview_truncated: metrics.preview_truncated,
            limit_compacted: metrics.limit_compacted,
            search_id: metrics.search_id,
            parent_search_id,
            top_hits: metrics.top_hits,
        };
        if let Err(error) = self.logger.write(&record) {
            eprintln!(
                "AWI MCP audit write failed at {}: {error:#}",
                self.logger.path.display()
            );
        }
    }
}

#[derive(Serialize)]
struct AuditRecord<'a> {
    schema_version: u8,
    timestamp_unix_ms: u64,
    pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_process: Option<&'a str>,
    sequence: u64,
    tool: &'a str,
    arguments: Value,
    arguments_blake3: &'a str,
    duration_ms: u128,
    status: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_message: Option<String>,
    response_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    text_content_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    structured_content_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_count_kind: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preview_truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit_compacted: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    search_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_search_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_hits: Option<Vec<AuditTopHit>>,
}

#[derive(Serialize)]
struct AuditTopHit {
    file_id: i64,
    score: f64,
    matched_lanes: Vec<String>,
}

struct ResponseMetrics {
    status: &'static str,
    error_code: Option<String>,
    error_message: Option<String>,
    response_bytes: usize,
    text_content_bytes: Option<usize>,
    structured_content_bytes: Option<usize>,
    result_count: Option<usize>,
    result_count_kind: Option<&'static str>,
    result_truncated: Option<bool>,
    preview_truncated: Option<bool>,
    limit_compacted: Option<bool>,
    search_id: Option<String>,
    top_hits: Option<Vec<AuditTopHit>>,
}

fn response_metrics(
    tool: &'static str,
    result: &Result<CallToolResult, McpError>,
) -> ResponseMetrics {
    match result {
        Err(error) => ResponseMetrics {
            status: "protocol_error",
            error_code: serde_json::to_value(error.code)
                .ok()
                .and_then(|value| value.as_i64())
                .map(|value| value.to_string()),
            error_message: Some(sanitize_text(error.message.as_ref())),
            response_bytes: serde_json::to_vec(error).map_or(0, |value| value.len()),
            text_content_bytes: None,
            structured_content_bytes: None,
            result_count: None,
            result_count_kind: None,
            result_truncated: None,
            preview_truncated: None,
            limit_compacted: None,
            search_id: None,
            top_hits: None,
        },
        Ok(result) => {
            let content = result.structured_content.as_ref();
            let is_error = result.is_error.unwrap_or(false);
            let (result_count, result_count_kind) = match tool {
                "workspace_search" => (
                    content
                        .and_then(|value| value.get("hits"))
                        .and_then(Value::as_array)
                        .map(Vec::len),
                    Some("hits"),
                ),
                "workspace_inspect" => (
                    content
                        .and_then(|value| value.get("symbols"))
                        .and_then(Value::as_array)
                        .map(Vec::len),
                    Some("symbols"),
                ),
                "workspace_query" => (
                    content
                        .and_then(|value| value.get("row_count"))
                        .and_then(Value::as_u64)
                        .and_then(|value| usize::try_from(value).ok()),
                    Some("rows"),
                ),
                _ => (None, None),
            };
            let result_count_kind = result_count.and(result_count_kind);
            ResponseMetrics {
                status: if is_error { "tool_error" } else { "ok" },
                error_code: if is_error {
                    content
                        .and_then(|value| value.pointer("/error/code"))
                        .map(value_string)
                } else {
                    None
                },
                error_message: if is_error {
                    content
                        .and_then(|value| value.pointer("/error/message"))
                        .map(value_string)
                        .map(|message| sanitize_text(&message))
                } else {
                    None
                },
                response_bytes: serde_json::to_vec(result).map_or(0, |value| value.len()),
                text_content_bytes: Some(
                    result
                        .content
                        .iter()
                        .filter_map(|block| block.as_text())
                        .map(|text| text.text.len())
                        .sum(),
                ),
                structured_content_bytes: content
                    .and_then(|value| serde_json::to_vec(value).ok())
                    .map(|value| value.len()),
                result_count,
                result_count_kind,
                result_truncated: content.and_then(|value| truncation_flag(tool, value)),
                preview_truncated: (tool == "workspace_search")
                    .then(|| {
                        content
                            .and_then(|value| value.get("previews_truncated"))
                            .and_then(Value::as_bool)
                    })
                    .flatten(),
                limit_compacted: (tool == "workspace_search")
                    .then(|| {
                        content
                            .and_then(|value| value.get("limit_compacted"))
                            .and_then(Value::as_bool)
                    })
                    .flatten(),
                search_id: (tool == "workspace_search")
                    .then(|| {
                        content
                            .and_then(|value| value.get("search_id"))
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .flatten(),
                top_hits: (tool == "workspace_search")
                    .then(|| content.and_then(audit_top_hits))
                    .flatten(),
            }
        }
    }
}

fn audit_top_hits(content: &Value) -> Option<Vec<AuditTopHit>> {
    Some(
        content
            .get("hits")?
            .as_array()?
            .iter()
            .filter_map(|hit| {
                Some(AuditTopHit {
                    file_id: hit.get("file_id")?.as_i64()?,
                    score: hit.get("score")?.as_f64()?,
                    matched_lanes: hit
                        .get("matched_lanes")?
                        .as_array()?
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect(),
                })
            })
            .collect(),
    )
}

fn truncation_flag(tool: &str, content: &Value) -> Option<bool> {
    match tool {
        "workspace_search" => ["previews_truncated", "limit_compacted"]
            .into_iter()
            .filter_map(|key| content.get(key).and_then(Value::as_bool))
            .reduce(|left, right| left || right),
        "workspace_inspect" => content.get("coverage").and_then(|coverage| {
            [
                "symbols_truncated",
                "dataset_columns_truncated",
                "dataset_schema_truncated",
            ]
            .into_iter()
            .filter_map(|key| coverage.get(key).and_then(Value::as_bool))
            .reduce(|left, right| left || right)
        }),
        "workspace_query" => content.get("truncated").and_then(Value::as_bool),
        _ => None,
    }
}

fn sanitize_value(value: Value, depth: usize) -> Value {
    if depth >= MAX_VALUE_DEPTH {
        return Value::String("[TRUNCATED:depth]".to_owned());
    }
    match value {
        Value::String(value) => Value::String(sanitize_text(&value)),
        Value::Array(values) => {
            let total = values.len();
            let mut retained = values
                .into_iter()
                .take(MAX_ARRAY_ITEMS)
                .map(|value| sanitize_value(value, depth + 1))
                .collect::<Vec<_>>();
            if total > retained.len() {
                retained.push(json!({"omitted_items": total - retained.len()}));
            }
            Value::Array(retained)
        }
        Value::Object(values) => {
            let total = values.len();
            let mut retained = Map::new();
            for (key, value) in values.into_iter().take(MAX_OBJECT_FIELDS) {
                let value = if is_sensitive_key(&key) {
                    Value::String("[REDACTED]".to_owned())
                } else {
                    sanitize_value(value, depth + 1)
                };
                retained.insert(key, value);
            }
            if total > retained.len() {
                retained.insert(
                    "_omitted_fields".to_owned(),
                    Value::from(total - retained.len()),
                );
            }
            Value::Object(retained)
        }
        value => value,
    }
}

fn sanitize_text(value: &str) -> String {
    let value = bearer_regex().replace_all(value, "${1}[REDACTED]");
    let value = assignment_regex().replace_all(&value, "$1$2[REDACTED]");
    let value = sk_token_regex().replace_all(&value, "[REDACTED]");
    let value = value.replace("[REDACTED] [REDACTED]", "[REDACTED]");
    truncate_chars(&value, MAX_STRING_CHARS)
}

fn bearer_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{8,}")
            .expect("valid bearer redaction regex")
    })
}

fn assignment_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(api[_-]?key|access[_-]?token|refresh[_-]?token|token|secret|password|authorization)\b(\s*(?:=|:)\s*)(?:"[^"]*"|'[^']*'|[^\s,;]+)"#,
        )
        .expect("valid credential assignment redaction regex")
    })
}

fn sk_token_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| {
        Regex::new(r"\bsk-[A-Za-z0-9_-]{8,}\b").expect("valid API token redaction regex")
    })
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace('-', "_");
    normalized == "authorization"
        || normalized == "password"
        || normalized == "secret"
        || normalized.ends_with("_secret")
        || normalized == "token"
        || normalized.ends_with("_token")
        || normalized == "api_key"
        || normalized.ends_with("_api_key")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    truncated.push_str("[TRUNCATED]");
    truncated
}

fn secure_append_file(path: &Path) -> Result<File> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && metadata.file_type().is_symlink()
    {
        anyhow::bail!("refusing symlink for AWI MCP audit file {}", path.display());
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("open AWI MCP audit file {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("set AWI MCP audit permissions on {}", path.display()))?;
    Ok(file)
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn parent_process_name() -> Option<String> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let parent_pid = status.lines().find_map(|line| {
        line.strip_prefix("PPid:")
            .map(str::trim)
            .and_then(|value| value.parse::<u32>().ok())
    })?;
    let name = fs::read_to_string(format!("/proc/{parent_pid}/comm")).ok()?;
    let name = sanitize_text(name.trim());
    (!name.is_empty()).then_some(name)
}

fn value_string(value: &Value) -> String {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use rmcp::model::CallToolResult;
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn writes_bounded_redacted_audit_record_with_private_permissions() {
        let fixture = tempdir().unwrap();
        let path = fixture.path().join("calls.jsonl");
        let logger = McpAuditLogger::open(&path).unwrap();
        let span = logger.span(
            "workspace_search",
            json!({
                "query": "authorization: Bearer abcdefghijk token=top-secret sk-abcdefghijk",
                "limit": 5
            }),
        );
        let result = Ok(CallToolResult::structured(json!({
            "search_id": "s-test-1",
            "hits": [{
                "file_id": 42,
                "path": "result.rs",
                "score": 1.25,
                "matched_lanes": ["exact_symbol", "symbol"]
            }],
            "previews_truncated": false,
            "limit_compacted": false
        })));
        span.finish(&result);

        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("abcdefghijk"));
        assert!(!text.contains("top-secret"));
        let record: Value = serde_json::from_str(text.trim()).unwrap();
        assert_eq!(record["tool"], "workspace_search");
        assert_eq!(record["schema_version"], 4);
        assert_eq!(record["status"], "ok");
        assert_eq!(record["result_count"], 1);
        assert_eq!(record["result_count_kind"], "hits");
        assert_eq!(record["result_truncated"], false);
        assert_eq!(record["preview_truncated"], false);
        assert_eq!(record["limit_compacted"], false);
        assert_eq!(record["search_id"], "s-test-1");
        assert_eq!(record["top_hits"][0]["file_id"], 42);
        assert_eq!(record["top_hits"][0]["score"], 1.25);
        assert_eq!(
            record["top_hits"][0]["matched_lanes"],
            json!(["exact_symbol", "symbol"])
        );
        assert!(record["client_process"].as_str().is_some());
        assert!(record["response_bytes"].as_u64().unwrap() > 0);
        assert!(record["text_content_bytes"].as_u64().unwrap() > 0);
        assert!(record["structured_content_bytes"].as_u64().unwrap() > 0);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn rotates_audit_log_at_configured_limit() {
        let fixture = tempdir().unwrap();
        let path = fixture.path().join("calls.jsonl");
        let logger = McpAuditLogger::open_with_limit(&path, 1_200).unwrap();
        for sequence in 0..12 {
            let span = logger.span(
                "workspace_search",
                json!({"query": format!("rotation query {sequence} {}", "x".repeat(120))}),
            );
            let result = Ok(CallToolResult::structured(json!({
                "hits": [],
                "previews_truncated": false,
                "limit_compacted": false
            })));
            span.finish(&result);
        }

        let rotated = sibling_path(&path, ".1");
        assert!(rotated.is_file());
        assert!(fs::metadata(&path).unwrap().len() <= 1_200);
        assert!(fs::metadata(&rotated).unwrap().len() <= 1_200);
    }

    #[test]
    fn records_redacted_tool_error_message() {
        let fixture = tempdir().unwrap();
        let path = fixture.path().join("calls.jsonl");
        let logger = McpAuditLogger::open(&path).unwrap();
        let span = logger.span(
            "workspace_inspect",
            json!({"path": "/missing", "parent_search_id": "s-test-1"}),
        );
        let result = Ok(CallToolResult::structured_error(json!({
            "error": {
                "code": "inspect_failed",
                "message": "authorization: Bearer abcdefghijk"
            }
        })));
        span.finish(&result);

        let record: Value =
            serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
        assert_eq!(record["parent_search_id"], "s-test-1");
        assert_eq!(record["error_code"], "inspect_failed");
        assert_eq!(record["error_message"], "authorization: [REDACTED]");
    }
}
