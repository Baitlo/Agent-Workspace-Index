use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use duckdb::types::{TimeUnit, Value as DuckValue};
use duckdb::{Config, Connection};
use serde_json::{Map, Number, Value, json};
use sqlparser::ast::{Expr, ObjectName, ObjectNamePart, Select, TableFactor, Visit, Visitor};
use sqlparser::dialect::DuckDbDialect;
use sqlparser::parser::Parser;

use crate::extract::extension;
use crate::model::{
    DatasetColumn, DatasetProfile, QueryColumn, QueryInput, QueryRequest, QueryResult, QuerySource,
};

const PROFILE_SAMPLE_SIZE: usize = 20_480;
const MAX_QUERY_ROWS: usize = 10_000;
const MAX_QUERY_BYTES: usize = 8 * 1024 * 1024;
const MAX_QUERY_TIMEOUT_MS: u64 = 30_000;
const DUCKDB_MEMORY_LIMIT: &str = "512MB";
const DUCKDB_THREADS: i64 = 2;

pub(crate) struct DuckDbProfiler {
    timeout: Duration,
    max_profile_bytes: u64,
}

impl DuckDbProfiler {
    pub(crate) fn new(timeout_seconds: u64, max_profile_bytes: u64) -> Self {
        Self {
            timeout: Duration::from_secs(timeout_seconds),
            max_profile_bytes,
        }
    }

    pub(crate) fn profile(&self, path: &Path, size_bytes: u64) -> Option<DatasetProfile> {
        let format = extension(path)?;
        if !matches!(
            format.as_str(),
            "csv" | "tsv" | "json" | "jsonl" | "ndjson" | "parquet"
        ) {
            return None;
        }
        if format != "parquet" && size_bytes > self.max_profile_bytes {
            return Some(DatasetProfile {
                format,
                status: "skipped_oversized".to_owned(),
                columns: Vec::new(),
                profiler: "embedded-duckdb".to_owned(),
                error: None,
            });
        }

        Some(match self.run_profile(path, &format) {
            Ok(columns) => DatasetProfile {
                format,
                status: "profiled".to_owned(),
                columns,
                profiler: "embedded-duckdb".to_owned(),
                error: None,
            },
            Err(error) => DatasetProfile {
                format,
                status: "profile_failed".to_owned(),
                columns: Vec::new(),
                profiler: "embedded-duckdb".to_owned(),
                error: Some(truncate_text(&format!("{error:#}"), 600)),
            },
        })
    }

    fn run_profile(&self, path: &Path, format: &str) -> Result<Vec<DatasetColumn>> {
        let path = path
            .canonicalize()
            .with_context(|| format!("resolve dataset {}", path.display()))?;
        let root = path
            .parent()
            .context("dataset path has no parent directory")?
            .to_owned();
        let connection = open_connection(&[root])?;

        run_with_timeout(&connection, self.timeout, || {
            register_source(&connection, "dataset", &path, format)?;
            let mut statement = connection.prepare("DESCRIBE SELECT * FROM dataset")?;
            let mut rows = statement.query([])?;
            let mut columns = Vec::new();
            while let Some(row) = rows.next()? {
                columns.push(DatasetColumn {
                    name: row.get(0)?,
                    data_type: row.get(1)?,
                    nullable: row
                        .get::<_, Option<String>>(2)?
                        .map(|value| value.eq_ignore_ascii_case("yes")),
                });
            }
            Ok(columns)
        })
    }
}

pub(crate) fn execute_query(request: &QueryRequest) -> Result<QueryResult> {
    validate_query_limits(request)?;
    let roots = canonical_roots(&request.roots)?;
    let sources = canonical_sources(&request.inputs, &roots)?;
    let aliases = sources
        .iter()
        .map(|source| source.alias.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    validate_read_only_sql(&request.sql, &aliases)?;
    let sql = request.sql.trim();
    let sql = sql.strip_suffix(';').unwrap_or(sql).trim_end();

    let connection = open_connection(&roots)?;
    let timeout = Duration::from_millis(request.timeout_ms);
    let started = Instant::now();
    let row_limit = request.max_rows;
    let wrapped_sql = format!(
        "SELECT * FROM ({}) AS awi_bounded_query LIMIT {}",
        sql,
        row_limit.saturating_add(1)
    );
    let mut result = run_with_timeout(&connection, timeout, || {
        for source in &sources {
            register_source(
                &connection,
                &source.alias,
                Path::new(&source.path),
                &source.format,
            )?;
        }
        execute_bounded_rows(
            &connection,
            &wrapped_sql,
            row_limit,
            request.max_bytes,
            &sources,
        )
    })?;
    result.elapsed_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    enforce_output_bytes(&mut result, request.max_bytes)?;
    Ok(result)
}

fn validate_query_limits(request: &QueryRequest) -> Result<()> {
    if request.roots.is_empty() {
        anyhow::bail!("workspace_query requires at least one allowed root");
    }
    if request.inputs.is_empty() {
        anyhow::bail!("workspace_query requires at least one input file");
    }
    if request.max_rows == 0 || request.max_rows > MAX_QUERY_ROWS {
        anyhow::bail!("max_rows must be between 1 and {MAX_QUERY_ROWS}");
    }
    if request.max_bytes == 0 || request.max_bytes > MAX_QUERY_BYTES {
        anyhow::bail!("max_bytes must be between 1 and {MAX_QUERY_BYTES}");
    }
    if request.timeout_ms == 0 || request.timeout_ms > MAX_QUERY_TIMEOUT_MS {
        anyhow::bail!("timeout_ms must be between 1 and {MAX_QUERY_TIMEOUT_MS}");
    }
    Ok(())
}

fn canonical_roots(roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut canonical = Vec::with_capacity(roots.len());
    for root in roots {
        let root = root
            .canonicalize()
            .with_context(|| format!("resolve allowed root {}", root.display()))?;
        if !root.is_dir() {
            anyhow::bail!("allowed root is not a directory: {}", root.display());
        }
        if !canonical.contains(&root) {
            canonical.push(root);
        }
    }
    Ok(canonical)
}

fn canonical_sources(inputs: &[QueryInput], roots: &[PathBuf]) -> Result<Vec<QuerySource>> {
    let mut aliases = HashSet::new();
    inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            let path = input
                .path
                .canonicalize()
                .with_context(|| format!("resolve query input {}", input.path.display()))?;
            if !path.is_file() {
                anyhow::bail!("query input is not a file: {}", path.display());
            }
            if !roots.iter().any(|root| path.starts_with(root)) {
                anyhow::bail!(
                    "query input {} is outside the allowed roots",
                    path.display()
                );
            }
            let format = extension(&path).context("query input has no supported extension")?;
            if !matches!(
                format.as_str(),
                "csv" | "tsv" | "json" | "jsonl" | "ndjson" | "parquet"
            ) {
                anyhow::bail!("unsupported query input format: {format}");
            }
            let alias = input
                .alias
                .clone()
                .unwrap_or_else(|| format!("data_{index}"));
            validate_alias(&alias)?;
            if !aliases.insert(alias.to_ascii_lowercase()) {
                anyhow::bail!("duplicate query input alias: {alias}");
            }
            Ok(QuerySource {
                alias,
                path: path.to_string_lossy().into_owned(),
                format,
            })
        })
        .collect()
}

fn validate_alias(alias: &str) -> Result<()> {
    let mut chars = alias.chars();
    if !chars
        .next()
        .is_some_and(|value| value == '_' || value.is_ascii_alphabetic())
        || !chars.all(|value| value == '_' || value.is_ascii_alphanumeric())
    {
        anyhow::bail!("query input alias must match [A-Za-z_][A-Za-z0-9_]*: {alias}");
    }
    Ok(())
}

fn validate_read_only_sql(sql: &str, aliases: &HashSet<String>) -> Result<()> {
    let statements = Parser::parse_sql(&DuckDbDialect {}, sql).context("parse DuckDB SQL")?;
    if statements.len() != 1 {
        anyhow::bail!("workspace_query accepts exactly one SQL statement");
    }
    if !matches!(
        statements.first(),
        Some(sqlparser::ast::Statement::Query(_))
    ) {
        anyhow::bail!("workspace_query accepts only SELECT or WITH queries");
    }

    let mut visitor = ReadOnlyVisitor {
        allowed_relations: aliases.clone(),
    };
    if let ControlFlow::Break(error) = statements.visit(&mut visitor) {
        anyhow::bail!("{error}");
    }
    Ok(())
}

struct ReadOnlyVisitor {
    allowed_relations: HashSet<String>,
}

impl Visitor for ReadOnlyVisitor {
    type Break = String;

    fn pre_visit_query(&mut self, query: &sqlparser::ast::Query) -> ControlFlow<Self::Break> {
        if let Some(with) = &query.with {
            self.allowed_relations.extend(
                with.cte_tables
                    .iter()
                    .map(|cte| cte.alias.name.value.to_ascii_lowercase()),
            );
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        if select.into.is_some() {
            return ControlFlow::Break("SELECT INTO is not allowed".to_owned());
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_relation(&mut self, relation: &ObjectName) -> ControlFlow<Self::Break> {
        let Some(name) = simple_object_name(relation) else {
            return ControlFlow::Break(format!(
                "qualified or functional relation is not allowed: {relation}"
            ));
        };
        if self.allowed_relations.contains(&name.to_ascii_lowercase()) {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(format!(
                "relation {name} is not one of the explicitly registered inputs"
            ))
        }
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        match factor {
            TableFactor::Table { args: None, .. }
            | TableFactor::Derived { .. }
            | TableFactor::NestedJoin { .. } => ControlFlow::Continue(()),
            _ => ControlFlow::Break(
                "table functions and external scans are not allowed; use an input alias".to_owned(),
            ),
        }
    }

    fn pre_visit_expr(&mut self, expression: &Expr) -> ControlFlow<Self::Break> {
        let Expr::Function(function) = expression else {
            return ControlFlow::Continue(());
        };
        let name = function.name.to_string().to_ascii_lowercase();
        const DENIED_FUNCTIONS: &[&str] = &[
            "getenv",
            "glob",
            "query",
            "query_table",
            "read_blob",
            "read_csv",
            "read_csv_auto",
            "read_json",
            "read_json_auto",
            "read_ndjson",
            "read_parquet",
            "read_text",
            "write_blob",
        ];
        if DENIED_FUNCTIONS.contains(&name.as_str()) {
            ControlFlow::Break(format!(
                "function {name} is not allowed; use an input alias"
            ))
        } else {
            ControlFlow::Continue(())
        }
    }
}

fn simple_object_name(name: &ObjectName) -> Option<&str> {
    match name.0.as_slice() {
        [ObjectNamePart::Identifier(identifier)] => Some(&identifier.value),
        _ => None,
    }
}

fn open_connection(roots: &[PathBuf]) -> Result<Connection> {
    let allowed_directories = format!(
        "[{}]",
        roots
            .iter()
            .map(|root| sql_string(&root.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(",")
    );
    let config = Config::default()
        .enable_autoload_extension(false)?
        .enable_object_cache(false)?
        .max_memory(DUCKDB_MEMORY_LIMIT)?
        .threads(DUCKDB_THREADS)?
        .with("allow_community_extensions", "false")?
        .with("allow_persistent_secrets", "false")?
        .with("allow_unsigned_extensions", "false")?
        .with("max_temp_directory_size", "0B")?;
    let connection =
        Connection::open_in_memory_with_flags(config).context("open embedded DuckDB")?;
    connection.execute_batch(&format!(
        "SET allowed_directories = {allowed_directories};
         SET enable_external_access = false;
         SET lock_configuration = true;"
    ))?;
    Ok(connection)
}

fn register_source(connection: &Connection, alias: &str, path: &Path, format: &str) -> Result<()> {
    let path = sql_string(&path.to_string_lossy());
    let relation = match format {
        "parquet" => format!("read_parquet({path})"),
        "csv" => format!("read_csv_auto({path}, sample_size={PROFILE_SAMPLE_SIZE})"),
        "tsv" => {
            format!("read_csv_auto({path}, delim='\\t', sample_size={PROFILE_SAMPLE_SIZE})")
        }
        "json" => format!("read_json_auto({path}, sample_size={PROFILE_SAMPLE_SIZE})"),
        "jsonl" | "ndjson" => format!(
            "read_json_auto({path}, format='newline_delimited', sample_size={PROFILE_SAMPLE_SIZE})"
        ),
        _ => anyhow::bail!("unsupported dataset format: {format}"),
    };
    connection
        .execute_batch(&format!(
            "CREATE TEMP VIEW {} AS SELECT * FROM {relation}",
            quote_identifier(alias)
        ))
        .with_context(|| format!("register query input {}", path))
}

fn execute_bounded_rows(
    connection: &Connection,
    sql: &str,
    row_limit: usize,
    max_bytes: usize,
    sources: &[QuerySource],
) -> Result<QueryResult> {
    let mut statement = connection.prepare(sql).context("prepare workspace query")?;
    let mut rows = statement.query([]).context("execute workspace query")?;
    let columns = {
        let statement = rows.as_ref().context("query statement unavailable")?;
        (0..statement.column_count())
            .map(|index| QueryColumn {
                name: statement
                    .column_name(index)
                    .map_or_else(|_| format!("column_{index}"), Clone::clone),
                data_type: format!("{:?}", statement.column_type(index)),
            })
            .collect::<Vec<_>>()
    };
    let mut output_rows = Vec::with_capacity(row_limit);
    let mut output_bytes = 0usize;
    let mut truncated = false;
    while let Some(row) = rows.next().context("fetch workspace query row")? {
        if output_rows.len() == row_limit {
            truncated = true;
            break;
        }
        let mut values = Vec::with_capacity(columns.len());
        for (index, column) in columns.iter().enumerate() {
            let value = row
                .get::<_, DuckValue>(index)
                .with_context(|| format!("decode query column {}", column.name))?;
            values.push(duck_value_to_json(value));
        }
        let row_bytes = serde_json::to_vec(&values)?.len();
        if output_bytes.saturating_add(row_bytes) > max_bytes {
            truncated = true;
            break;
        }
        output_bytes += row_bytes;
        output_rows.push(values);
    }

    Ok(QueryResult {
        columns,
        row_count: output_rows.len(),
        rows: output_rows,
        sources: sources.to_owned(),
        truncated,
        elapsed_ms: 0,
    })
}

fn enforce_output_bytes(result: &mut QueryResult, max_bytes: usize) -> Result<()> {
    while serde_json::to_vec(result)?.len() > max_bytes {
        if result.rows.pop().is_none() {
            anyhow::bail!("query metadata exceeds max_bytes={max_bytes}");
        }
        result.truncated = true;
        result.row_count = result.rows.len();
    }
    Ok(())
}

fn run_with_timeout<T>(
    connection: &Connection,
    timeout: Duration,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let interrupt = connection.interrupt_handle();
    let (done_tx, done_rx) = mpsc::channel();
    let watchdog = thread::spawn(move || {
        if done_rx.recv_timeout(timeout).is_err() {
            interrupt.interrupt();
            true
        } else {
            false
        }
    });
    let result = operation();
    let _ = done_tx.send(());
    let timed_out = watchdog.join().unwrap_or(true);
    if timed_out {
        anyhow::bail!("DuckDB query timed out after {timeout:?}");
    }
    result
}

fn duck_value_to_json(value: DuckValue) -> Value {
    match value {
        DuckValue::Null => Value::Null,
        DuckValue::Boolean(value) => Value::Bool(value),
        DuckValue::TinyInt(value) => json!(value),
        DuckValue::SmallInt(value) => json!(value),
        DuckValue::Int(value) => json!(value),
        DuckValue::BigInt(value) => json!(value),
        DuckValue::HugeInt(value) => Value::String(value.to_string()),
        DuckValue::UHugeInt(value) => Value::String(value.to_string()),
        DuckValue::UTinyInt(value) => json!(value),
        DuckValue::USmallInt(value) => json!(value),
        DuckValue::UInt(value) => json!(value),
        DuckValue::UBigInt(value) => json!(value),
        DuckValue::Float(value) => Number::from_f64(f64::from(value))
            .map(Value::Number)
            .unwrap_or(Value::Null),
        DuckValue::Double(value) => Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        DuckValue::Decimal(value) => Value::String(value.to_string()),
        DuckValue::Timestamp(unit, value) => typed_time("timestamp", unit, value),
        DuckValue::Text(value) => Value::String(value),
        DuckValue::Blob(value) => Value::String(format!("hex:{}", hex(&value))),
        DuckValue::Geometry(value) => Value::String(format!("wkb:{}", hex(&value))),
        DuckValue::Date32(value) => json!({"date32_days": value}),
        DuckValue::Time64(unit, value) => typed_time("time", unit, value),
        DuckValue::Interval {
            months,
            days,
            nanos,
        } => json!({"months": months, "days": days, "nanos": nanos}),
        DuckValue::List(values) | DuckValue::Array(values) => {
            Value::Array(values.into_iter().map(duck_value_to_json).collect())
        }
        DuckValue::Enum(value) => Value::String(value),
        DuckValue::Struct(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), duck_value_to_json(value.clone())))
                .collect::<Map<_, _>>(),
        ),
        DuckValue::Map(values) => Value::Array(
            values
                .iter()
                .map(|(key, value)| {
                    json!({
                        "key": duck_value_to_json(key.clone()),
                        "value": duck_value_to_json(value.clone())
                    })
                })
                .collect(),
        ),
        DuckValue::Union(value) => duck_value_to_json(*value),
        _ => Value::String(format!("{value:?}")),
    }
}

fn typed_time(kind: &str, unit: TimeUnit, value: i64) -> Value {
    json!({
        "kind": kind,
        "unit": format!("{unit:?}").to_ascii_lowercase(),
        "value": value
    })
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(DIGITS[usize::from(byte >> 4)] as char);
        output.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    output
}

fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn sql_string(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn truncate_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn profiles_csv_with_embedded_duckdb() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("metrics.csv");
        fs::write(&path, "model,score\nawi,0.95\n").unwrap();
        let profile = DuckDbProfiler::new(5, 1024 * 1024)
            .profile(&path, fs::metadata(&path).unwrap().len())
            .unwrap();
        assert_eq!(profile.status, "profiled", "{:?}", profile.error);
        assert_eq!(profile.profiler, "embedded-duckdb");
        assert!(profile.columns.iter().any(|column| column.name == "model"));
        assert!(profile.columns.iter().any(|column| column.name == "score"));
    }

    #[test]
    fn queries_registered_input_with_limits() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("metrics.csv");
        fs::write(&path, "model,score\nawi,0.95\nother,0.80\n").unwrap();
        let result = execute_query(&QueryRequest {
            sql: "SELECT model, score FROM metrics ORDER BY score DESC;".to_owned(),
            roots: vec![directory.path().to_owned()],
            inputs: vec![QueryInput {
                path,
                alias: Some("metrics".to_owned()),
            }],
            max_rows: 1,
            max_bytes: 1024 * 1024,
            timeout_ms: 5_000,
        })
        .unwrap();
        assert_eq!(result.row_count, 1);
        assert!(result.truncated);
        assert_eq!(result.rows[0][0], "awi");
    }

    #[test]
    fn bounds_output_bytes_while_rows_are_decoded() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("large.csv");
        let value = "x".repeat(800);
        fs::write(&path, format!("value\n{value}\n{value}\n")).unwrap();
        let result = execute_query(&QueryRequest {
            sql: "SELECT value FROM data".to_owned(),
            roots: vec![directory.path().to_owned()],
            inputs: vec![QueryInput {
                path,
                alias: Some("data".to_owned()),
            }],
            max_rows: 10,
            max_bytes: 1_024,
            timeout_ms: 5_000,
        })
        .unwrap();
        assert!(result.truncated);
        assert!(serde_json::to_vec(&result).unwrap().len() <= 1_024);
    }

    #[test]
    fn rejects_writes_external_scans_and_paths_outside_roots() {
        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let path = directory.path().join("metrics.csv");
        let outside_path = outside.path().join("secret.csv");
        fs::write(&path, "model,score\nawi,0.95\n").unwrap();
        fs::write(&outside_path, "secret\nvalue\n").unwrap();
        let base = QueryRequest {
            sql: "SELECT * FROM metrics".to_owned(),
            roots: vec![directory.path().to_owned()],
            inputs: vec![QueryInput {
                path,
                alias: Some("metrics".to_owned()),
            }],
            max_rows: 10,
            max_bytes: 1024 * 1024,
            timeout_ms: 5_000,
        };

        let mut write = base.clone();
        write.sql = "CREATE TABLE stolen AS SELECT * FROM metrics".to_owned();
        assert!(
            execute_query(&write)
                .unwrap_err()
                .to_string()
                .contains("only SELECT")
        );

        let mut external = base.clone();
        external.sql = "SELECT * FROM read_csv_auto('/etc/passwd')".to_owned();
        assert!(
            execute_query(&external)
                .unwrap_err()
                .to_string()
                .contains("not allowed")
        );

        let mut escaped = base;
        escaped.inputs[0].path = outside_path;
        assert!(
            execute_query(&escaped)
                .unwrap_err()
                .to_string()
                .contains("outside the allowed roots")
        );
    }

    #[test]
    fn interrupts_long_running_duckdb_queries() {
        let directory = tempdir().unwrap();
        let connection = open_connection(&[directory.path().to_owned()]).unwrap();
        let started = Instant::now();
        let error = run_with_timeout(&connection, Duration::from_millis(50), || {
            connection
                .query_row(
                    "SELECT count(*) FROM range(10000000) t1, range(1000000) t2",
                    [],
                    |_| Ok(()),
                )
                .context("run intentionally expensive query")
        })
        .unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "DuckDB interrupt exceeded the bounded timeout grace period"
        );
    }
}
