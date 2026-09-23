use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use awi::benchmark::{evaluate as evaluate_retrieval, load_gold};
use awi::daemon::{default_socket_path, serve_with_snapshots, try_request};
use awi::protocol::Request;
use awi::{
    IndexOptions, IndexReport, IndexStatus, InspectResult, IntegrationClient, IntegrationOptions,
    NotifyReport, PublisherConfig, QueryInput, QueryRequest, QueryResult, SearchHit,
    SemanticBuildReport, WorkspaceIndex, default_server_spec, integrate as integrate_clients,
    render_human, watch as watch_publisher,
};
use clap::{Parser, Subcommand};
use serde::Serialize;
use serde::de::DeserializeOwned;

#[derive(Debug, Parser)]
#[command(name = "awi", version, about = "Agent Workspace Index")]
struct Cli {
    #[arg(long, env = "AWI_INDEX_DIR", default_value = ".awi-index")]
    index_dir: PathBuf,

    #[arg(long, env = "AWI_SOCKET", global = true)]
    socket: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Incrementally index one workspace directory, AGENTS.md, or SKILL.md root.
    Index {
        root: PathBuf,

        #[arg(long, default_value_t = 4)]
        max_content_mib: u64,

        #[arg(long, default_value_t = 256)]
        max_profile_mib: u64,

        #[arg(long, default_value_t = 15)]
        duckdb_timeout_seconds: u64,

        #[arg(long)]
        json: bool,
    },

    /// Reconcile one directory or Agent document root with a complete incremental scan.
    Reconcile {
        root: PathBuf,

        #[arg(long)]
        publish_dir: Option<PathBuf>,

        #[arg(long, default_value_t = 4)]
        max_content_mib: u64,

        #[arg(long, default_value_t = 256)]
        max_profile_mib: u64,

        #[arg(long, default_value_t = 15)]
        duckdb_timeout_seconds: u64,

        #[arg(long)]
        json: bool,
    },

    /// Discover and index memory for this project across supported Agent clients.
    Memory {
        #[arg(long, default_value = ".")]
        project_root: PathBuf,

        /// Include raw chat/session history. Curated memory is the default.
        #[arg(long)]
        include_raw: bool,

        #[arg(long, default_value_t = 4)]
        max_content_mib: u64,

        #[arg(long, default_value_t = 256)]
        max_profile_mib: u64,

        #[arg(long, default_value_t = 15)]
        duckdb_timeout_seconds: u64,

        #[arg(long)]
        json: bool,
    },

    /// Continuously reconcile roots and auto-publish immutable snapshots on change.
    Watch {
        /// Roots to reconcile. Defaults to every root already in the catalog.
        #[arg(long = "root")]
        roots: Vec<PathBuf>,

        /// Shared publication directory that snapshot-mode readers follow.
        #[arg(long, env = "AWI_SNAPSHOT_SOURCE")]
        publish_dir: PathBuf,

        /// Delay between reconcile cycles in milliseconds.
        #[arg(long, default_value_t = 5_000)]
        interval_ms: u64,

        /// Quiet period after a filesystem event before reconciling, in milliseconds.
        #[arg(long, default_value_t = 500)]
        debounce_ms: u64,

        /// Published generations to retain, including the active one.
        #[arg(long, default_value_t = 3)]
        retain: usize,

        #[arg(long, default_value_t = 4)]
        max_content_mib: u64,

        #[arg(long, default_value_t = 256)]
        max_profile_mib: u64,

        #[arg(long, default_value_t = 15)]
        duckdb_timeout_seconds: u64,
    },

    /// Index explicitly changed or deleted files beneath known roots.
    Notify {
        #[arg(required = true)]
        paths: Vec<PathBuf>,

        #[arg(long, default_value_t = 4)]
        max_content_mib: u64,

        #[arg(long, default_value_t = 256)]
        max_profile_mib: u64,

        #[arg(long, default_value_t = 15)]
        duckdb_timeout_seconds: u64,

        #[arg(long)]
        json: bool,
    },

    /// Search paths, text, symbols, Agent knowledge, and dataset schemas.
    Search {
        query: String,

        #[arg(long, default_value_t = 10)]
        limit: usize,

        #[arg(long = "root")]
        roots: Vec<PathBuf>,

        #[arg(long = "kind")]
        kinds: Vec<String>,

        #[arg(long)]
        path_prefix: Option<String>,

        /// File or directory used to resolve applicable AGENTS.md scopes.
        #[arg(long)]
        context_path: Option<PathBuf>,

        #[arg(long)]
        json: bool,
    },

    /// Inspect one indexed path.
    Inspect {
        path: PathBuf,

        #[arg(long, default_value_t = 1)]
        line_start: usize,

        #[arg(long, default_value_t = 120)]
        max_lines: usize,

        #[arg(long, default_value_t = 32 * 1024)]
        max_chars: usize,

        #[arg(long)]
        json: bool,
    },

    /// Run bounded read-only SQL over explicit structured files.
    Query {
        #[arg(long)]
        sql: String,

        #[arg(long = "root", required = true)]
        roots: Vec<PathBuf>,

        #[arg(long = "file", required = true)]
        files: Vec<PathBuf>,

        #[arg(long, default_value_t = 100)]
        max_rows: usize,

        #[arg(long, default_value_t = 1024 * 1024)]
        max_bytes: usize,

        #[arg(long, default_value_t = 10_000)]
        timeout_ms: u64,
    },

    /// Evaluate retrieval quality and latency against a reviewed gold set.
    Benchmark {
        #[arg(long)]
        gold: PathBuf,

        #[arg(long)]
        output: Option<PathBuf>,

        #[arg(long)]
        allow_draft: bool,
    },

    /// Rebuild the configured semantic index from the current catalog.
    SemanticBuild {
        #[arg(long)]
        publish_dir: Option<PathBuf>,

        /// Reuse current file generations already present in LanceDB.
        #[arg(long)]
        resume: bool,

        #[arg(long, default_value_t = 4)]
        max_content_mib: u64,

        #[arg(long)]
        json: bool,
    },

    /// Show catalog and generation health.
    Status {
        #[arg(long)]
        json: bool,
    },

    /// Run the persistent local AWI daemon.
    Serve {
        /// Shared snapshot publication directory to follow.
        #[arg(long, env = "AWI_SNAPSHOT_SOURCE")]
        snapshot_source: Option<PathBuf>,

        #[arg(long, default_value_t = 5_000)]
        snapshot_poll_ms: u64,
    },

    /// Check whether the AWI daemon is reachable.
    Ping {
        #[arg(long)]
        json: bool,
    },

    /// Stop the AWI daemon cleanly.
    Stop {
        #[arg(long)]
        json: bool,
    },

    /// Register AWI as an MCP server across supported Agent clients.
    Integrate {
        /// Client(s) to configure: all, codex, gemini, claude, copilot, trae,
        /// zcode, kimi, opencode, pi, cursor, windsurf, qwen, cline, zed,
        /// amazon-q, crush.
        /// Defaults to all detected clients.
        #[arg(long = "client", value_delimiter = ',')]
        clients: Vec<String>,

        /// Override the MCP server command. Defaults to this AWI executable with
        /// the absolute --index-dir and mcp arguments.
        #[arg(long)]
        server_command: Option<PathBuf>,

        /// Argument passed to the overridden MCP server command. Repeat as needed.
        #[arg(long = "server-arg", allow_hyphen_values = true)]
        server_args: Vec<String>,

        /// Project root used for TraeCode's .trae/mcp.json.
        #[arg(long, default_value = ".")]
        project_root: PathBuf,

        /// Report intended changes without modifying client configuration.
        #[arg(long)]
        dry_run: bool,

        #[arg(long)]
        json: bool,
    },

    /// Serve workspace_search, workspace_inspect, and workspace_query over MCP stdio.
    Mcp {
        /// Append bounded JSONL records for actual MCP tool calls.
        #[arg(long, env = "AWI_MCP_AUDIT_LOG")]
        audit_log: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let socket = cli
        .socket
        .unwrap_or_else(|| default_socket_path(&cli.index_dir));

    match cli.command {
        Command::Index {
            root,
            max_content_mib,
            max_profile_mib,
            duckdb_timeout_seconds,
            json,
        } => {
            let options = IndexOptions {
                max_content_bytes: mib(max_content_mib),
                max_profile_bytes: mib(max_profile_mib),
                duckdb_timeout_seconds,
            };
            let request = Request::Index {
                root: root.clone(),
                options: options.clone(),
            };
            let report: IndexReport =
                execute(&cli.index_dir, &socket, &request, move |workspace| {
                    workspace.index_root(root, &options)
                })?;
            if json {
                print_json(&report)?;
            } else {
                println!(
                    "generation={} discovered={} indexed={} unchanged={} deleted={} metadata_only={} failed={}",
                    report.generation,
                    report.discovered,
                    report.indexed,
                    report.unchanged,
                    report.deleted,
                    report.metadata_only,
                    report.failed
                );
            }
        }
        Command::Reconcile {
            root,
            publish_dir,
            max_content_mib,
            max_profile_mib,
            duckdb_timeout_seconds,
            json,
        } => {
            let options = IndexOptions {
                max_content_bytes: mib(max_content_mib),
                max_profile_bytes: mib(max_profile_mib),
                duckdb_timeout_seconds,
            };
            let request = Request::Index {
                root: root.clone(),
                options: options.clone(),
            };
            let report: IndexReport =
                execute(&cli.index_dir, &socket, &request, move |workspace| {
                    workspace.index_root(root, &options)
                })?;
            let snapshot = if let Some(publish_dir) = publish_dir {
                Some(WorkspaceIndex::open(&cli.index_dir)?.publish_snapshot(&publish_dir)?)
            } else {
                None
            };
            if json {
                print_json(&serde_json::json!({
                    "report": report,
                    "snapshot": snapshot
                }))?;
            } else {
                println!(
                    "generation={} discovered={} indexed={} unchanged={} deleted={} metadata_only={} failed={}",
                    report.generation,
                    report.discovered,
                    report.indexed,
                    report.unchanged,
                    report.deleted,
                    report.metadata_only,
                    report.failed
                );
                if let Some(snapshot) = snapshot {
                    println!(
                        "published_generation={} files={}",
                        snapshot.generation,
                        snapshot.files.len()
                    );
                }
            }
        }
        Command::Memory {
            project_root,
            include_raw,
            max_content_mib,
            max_profile_mib,
            duckdb_timeout_seconds,
            json,
        } => {
            let options = IndexOptions {
                max_content_bytes: mib(max_content_mib),
                max_profile_bytes: mib(max_profile_mib),
                duckdb_timeout_seconds,
            };
            let mut workspace = WorkspaceIndex::open(&cli.index_dir)
                .with_context(|| format!("open AWI index {}", cli.index_dir.display()))?;
            let report = workspace.index_agent_memories(project_root, include_raw, &options)?;
            if json {
                print_json(&report)?;
            } else {
                println!(
                    "sources={} files={} indexed={} unchanged={} deleted={} metadata_only={} failed={} raw={}",
                    report.sources.len(),
                    report
                        .reports
                        .iter()
                        .map(|item| item.discovered)
                        .sum::<u64>(),
                    report.reports.iter().map(|item| item.indexed).sum::<u64>(),
                    report
                        .reports
                        .iter()
                        .map(|item| item.unchanged)
                        .sum::<u64>(),
                    report.reports.iter().map(|item| item.deleted).sum::<u64>(),
                    report
                        .reports
                        .iter()
                        .map(|item| item.metadata_only)
                        .sum::<u64>(),
                    report.reports.iter().map(|item| item.failed).sum::<u64>(),
                    report.include_raw
                );
            }
        }
        Command::Watch {
            roots,
            publish_dir,
            interval_ms,
            debounce_ms,
            retain,
            max_content_mib,
            max_profile_mib,
            duckdb_timeout_seconds,
        } => {
            let config = PublisherConfig {
                publish_dir,
                interval: Duration::from_millis(interval_ms),
                debounce: Duration::from_millis(debounce_ms),
                retain,
                options: IndexOptions {
                    max_content_bytes: mib(max_content_mib),
                    max_profile_bytes: mib(max_profile_mib),
                    duckdb_timeout_seconds,
                },
            };
            watch_publisher(&cli.index_dir, &roots, &config)?;
        }
        Command::Notify {
            paths,
            max_content_mib,
            max_profile_mib,
            duckdb_timeout_seconds,
            json,
        } => {
            let options = IndexOptions {
                max_content_bytes: mib(max_content_mib),
                max_profile_bytes: mib(max_profile_mib),
                duckdb_timeout_seconds,
            };
            let request = Request::Notify {
                paths: paths.clone(),
                options: options.clone(),
            };
            let report: NotifyReport =
                execute(&cli.index_dir, &socket, &request, move |workspace| {
                    workspace.notify_paths(&paths, &options)
                })?;
            if json {
                print_json(&report)?;
            } else {
                println!(
                    "requested={} roots={} indexed={} unchanged={} deleted={} failed={}",
                    report.requested,
                    report.roots.len(),
                    report.roots.iter().map(|item| item.indexed).sum::<u64>(),
                    report.roots.iter().map(|item| item.unchanged).sum::<u64>(),
                    report.roots.iter().map(|item| item.deleted).sum::<u64>(),
                    report.roots.iter().map(|item| item.failed).sum::<u64>()
                );
            }
        }
        Command::Search {
            query,
            limit,
            roots,
            kinds,
            path_prefix,
            context_path,
            json,
        } => {
            let request = Request::Search {
                query: query.clone(),
                limit,
                roots: roots.clone(),
                kinds: kinds.clone(),
                path_prefix: path_prefix.clone(),
                context_path: context_path.clone(),
            };
            let hits: Vec<SearchHit> =
                execute(&cli.index_dir, &socket, &request, move |workspace| {
                    workspace.search_filtered(
                        &query,
                        limit,
                        &roots,
                        &kinds,
                        path_prefix.as_deref(),
                        context_path.as_deref(),
                    )
                })?;
            if json {
                print_json(&hits)?;
            } else {
                for hit in hits {
                    println!(
                        "{:.6}\t{}\t{}\t{}",
                        hit.score,
                        hit.matched_lanes.join(","),
                        hit.kind.as_str(),
                        hit.path
                    );
                    if !hit.preview.is_empty() {
                        println!("  {}", single_line(&hit.preview));
                    }
                }
            }
        }
        Command::Inspect {
            path,
            line_start,
            max_lines,
            max_chars,
            json,
        } => {
            let request = Request::Inspect {
                path: path.clone(),
                start_line: line_start,
                max_lines,
                max_chars,
            };
            let result: InspectResult =
                execute(&cli.index_dir, &socket, &request, move |workspace| {
                    workspace.inspect_excerpt(path, line_start, max_lines, max_chars)
                })?;
            if json {
                print_json(&result)?;
            } else {
                println!("path: {}", result.file.absolute_path);
                println!("kind: {}", result.file.kind.as_str());
                println!("size_bytes: {}", result.file.size_bytes);
                println!("generation: {}", result.file.generation);
                println!("extraction_status: {}", result.file.extraction_status);
                if let Some(dataset) = result.dataset {
                    println!("dataset_status: {}", dataset.status);
                    for column in dataset.columns {
                        println!("column: {}\t{}", column.name, column.data_type);
                    }
                }
                if let Some(memory) = result.memory {
                    println!("memory_agent: {}", memory.agent);
                    println!("memory_layer: {}", memory.layer.as_str());
                    if let Some(workspace_root) = memory.workspace_root {
                        println!("memory_workspace: {}", workspace_root.display());
                    }
                    if let Some(session_id) = memory.session_id {
                        println!("memory_session: {session_id}");
                    }
                }
                for symbol in result.symbols {
                    println!(
                        "symbol: {}\t{}\t{}:{}-{}",
                        symbol.kind,
                        symbol.name,
                        result.file.relative_path,
                        symbol.line_start,
                        symbol.line_end
                    );
                }
                if let Some(content) = result.content {
                    print!("{}", content.text);
                }
            }
        }
        Command::Query {
            sql,
            roots,
            files,
            max_rows,
            max_bytes,
            timeout_ms,
        } => {
            let query = QueryRequest {
                sql,
                roots,
                inputs: files
                    .into_iter()
                    .enumerate()
                    .map(|(index, path)| QueryInput {
                        path,
                        alias: Some(format!("data_{index}")),
                    })
                    .collect(),
                max_rows,
                max_bytes,
                timeout_ms,
            };
            let request = Request::Query {
                request: query.clone(),
            };
            let result: QueryResult =
                execute(&cli.index_dir, &socket, &request, move |workspace| {
                    workspace.query(&query)
                })?;
            print_json(&result)?;
        }
        Command::Benchmark {
            gold,
            output,
            allow_draft,
        } => {
            let gold = load_gold(&gold, allow_draft)?;
            let workspace = WorkspaceIndex::open(&cli.index_dir)
                .with_context(|| format!("open AWI index {}", cli.index_dir.display()))?;
            workspace
                .warm_semantic()
                .context("warm semantic retrieval before benchmark")?;
            let semantic = workspace.status()?.semantic;
            if semantic.enabled && !semantic.available {
                anyhow::bail!(
                    "semantic retrieval is configured but unavailable: {}",
                    semantic
                        .error
                        .unwrap_or_else(|| "semantic snapshot is missing".to_owned())
                );
            }
            let report = evaluate_retrieval(&workspace, &gold)?;
            if let Some(output) = output {
                if let Some(parent) = output.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&output, serde_json::to_vec_pretty(&report)?)
                    .with_context(|| format!("write benchmark report {}", output.display()))?;
            }
            print_json(&report)?;
        }
        Command::SemanticBuild {
            publish_dir,
            resume,
            max_content_mib,
            json,
        } => {
            let options = IndexOptions {
                max_content_bytes: mib(max_content_mib),
                ..IndexOptions::default()
            };
            let mut workspace = WorkspaceIndex::open(&cli.index_dir)
                .with_context(|| format!("open AWI index {}", cli.index_dir.display()))?;
            let report: SemanticBuildReport = workspace.rebuild_semantic(&options, resume)?;
            let snapshot = if let Some(publish_dir) = publish_dir {
                Some(workspace.publish_snapshot(publish_dir)?)
            } else {
                None
            };
            if json {
                print_json(&serde_json::json!({
                    "report": report,
                    "snapshot": snapshot
                }))?;
            } else {
                println!(
                    "generation={} files={} reused_files={} chunks={} skipped={}",
                    report.generation,
                    report.files,
                    report.reused_files,
                    report.chunks,
                    report.skipped
                );
                if let Some(snapshot) = snapshot {
                    println!(
                        "published_generation={} files={}",
                        snapshot.generation,
                        snapshot.files.len()
                    );
                }
            }
        }
        Command::Status { json } => {
            let status: IndexStatus =
                execute(&cli.index_dir, &socket, &Request::Status, |workspace| {
                    workspace.status()
                })?;
            if json {
                print_json(&status)?;
            } else {
                println!(
                    "completed_generation={:?} running={} failed={} active_files={} deleted_files={} symbols={} datasets={} agent_documents={} agent_memories={} failures={} semantic_enabled={} semantic_available={} semantic_generation={:?} semantic_files={} semantic_chunks={}",
                    status.completed_generation,
                    status.running_generations,
                    status.failed_generations,
                    status.active_files,
                    status.deleted_files,
                    status.symbols,
                    status.datasets,
                    status.agent_documents,
                    status.agent_memories,
                    status.failures,
                    status.semantic.enabled,
                    status.semantic.available,
                    status.semantic.generation,
                    status.semantic.files,
                    status.semantic.chunks
                );
            }
        }
        Command::Serve {
            snapshot_source,
            snapshot_poll_ms,
        } => serve_with_snapshots(
            &cli.index_dir,
            &socket,
            snapshot_source.as_deref(),
            Duration::from_millis(snapshot_poll_ms),
        )?,
        Command::Ping { json } => {
            let response = try_request(&socket, &Request::Ping)?
                .with_context(|| format!("AWI daemon is not running at {}", socket.display()))?;
            if json {
                print_json(&response)?;
            } else {
                println!("AWI daemon ready at {}", socket.display());
            }
        }
        Command::Stop { json } => {
            let response = try_request(&socket, &Request::Shutdown)?
                .with_context(|| format!("AWI daemon is not running at {}", socket.display()))?;
            if json {
                print_json(&response)?;
            } else {
                println!("AWI daemon stopped");
            }
        }
        Command::Integrate {
            clients,
            server_command,
            server_args,
            project_root,
            dry_run,
            json,
        } => {
            let clients = clients
                .iter()
                .map(|client| client.parse::<IntegrationClient>())
                .collect::<Result<Vec<_>>>()?;
            let server =
                default_server_spec(&cli.index_dir, server_command.as_deref(), &server_args)?;
            let project_root = if project_root.is_absolute() {
                project_root
            } else {
                std::env::current_dir()?.join(project_root)
            };
            let report = integrate_clients(&IntegrationOptions {
                clients,
                server,
                project_root,
                dry_run,
            })?;
            if json {
                print_json(&report)?;
            } else {
                println!("{}", render_human(&report));
            }
            if report.has_failures() {
                anyhow::bail!("one or more MCP client integrations failed");
            }
        }
        Command::Mcp { audit_log } => {
            awi::mcp::serve_stdio(cli.index_dir, socket, audit_log)?;
        }
    }

    Ok(())
}

fn execute<T>(
    index_dir: &std::path::Path,
    socket: &std::path::Path,
    request: &Request,
    local: impl FnOnce(&mut WorkspaceIndex) -> Result<T>,
) -> Result<T>
where
    T: Serialize + DeserializeOwned,
{
    if let Some(value) = try_request(socket, request)? {
        return serde_json::from_value(value).context("decode AWI daemon result");
    }
    let mut workspace = WorkspaceIndex::open(index_dir)
        .with_context(|| format!("open AWI index {}", index_dir.display()))?;
    local(&mut workspace)
}

fn mib(value: u64) -> u64 {
    value.saturating_mul(1024 * 1024)
}

fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn single_line(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}
