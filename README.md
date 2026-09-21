# AWI

AWI (Agent Workspace Index) is a local-first retrieval layer for coding agents.
It indexes source code, SQL, documents, logs, JSON/JSONL, CSV/TSV, and Parquet
through one bounded CLI and MCP surface.

## Capabilities

- Tantivy hybrid retrieval over paths, text, symbols, and dataset schemas.
- Tree-sitter symbol extraction for Rust, Python, and Go.
- Embedded, read-only DuckDB queries over explicitly allowlisted files.
- SQLite catalog checks that reject stale search results.
- NFS-safe `notify` and `reconcile` update paths.
- Immutable generation snapshots with atomic daemon activation.
- MCP tools: `workspace_search`, `workspace_inspect`, and `workspace_query`.

## Build And Test

```bash
export CARGO_TARGET_DIR=/tmp/awi-target
cargo build --release
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

## Basic Usage

```bash
# Build or refresh an index.
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace --json

# Run the persistent daemon.
awi --index-dir /tmp/my-awi-index serve

# Search and inspect.
awi --index-dir /tmp/my-awi-index search "workspace query" --limit 10 --json
awi --index-dir /tmp/my-awi-index inspect /path/to/file --json

# Start the MCP stdio adapter.
awi --index-dir /tmp/my-awi-index mcp

# Persist actual MCP tool calls for later retrieval analysis.
awi --index-dir /tmp/my-awi-index mcp \
  --audit-log /shared/awi/runtime/calls.jsonl
```

For NFS-backed workspaces, build mutable indexes on local storage and publish
immutable snapshots:

```bash
awi --index-dir /tmp/my-awi-index reconcile /path/to/workspace \
  --publish-dir /shared/awi-publication --json

awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

### Automatic Update Chain

To keep the shared publication current without manual reconciles, run the
persistent producer alongside the reader. The producer reconciles roots and
publishes a new immutable snapshot only when content changed; the
snapshot-following reader atomically switches to each new generation on its next
request. This closes the loop end to end: edit a file, and the reader reflects
it automatically.

Local-disk roots are watched in real time (inotify), so edits publish within the
debounce window. Remote roots (NFS and similar, detected via `/proc/mounts`) and
a periodic safety-net tick every `--interval-ms` drive the rest, because
filesystem events are not reliable for remote writes. If the watcher cannot
start, the producer degrades cleanly to pure periodic reconcile. Access and
metadata-only events are ignored to prevent self-triggered scans; append-heavy
`.log`, `.jsonl`, `.ndjson`, `.csv`, `.tsv`, and `.parquet` updates are deferred
to the periodic pass instead of rebuilding a snapshot for every write.

```bash
# Producer: watch local roots live, reconcile every root at most every 5s,
# coalesce edit bursts over 500ms, auto-publish on change, retain 3 generations.
awi --index-dir /tmp/awi-writer watch \
  --publish-dir /shared/awi-publication \
  --interval-ms 5000 --debounce-ms 500 --retain 3

# Reader: follow the publication and auto-activate new generations.
awi --index-dir /tmp/awi-reader serve \
  --snapshot-source /shared/awi-publication
```

`watch` defaults to every root already registered in the catalog; pass
`--root <path>` one or more times to restrict the set. Retention pruning removes
older generations after each publish and never deletes the generation the
pointer currently references, so the shared directory cannot grow without bound.

For Codex, `scripts/awi-mcp-snapshot-wrapper.sh` starts or reuses one local
snapshot daemon before launching the stdio adapter. Configure
`AWI_SNAPSHOT_SOURCE` and `AWI_MCP_AUDIT_LOG`, then register the wrapper as a
global MCP server.

### One-command Agent Integration

`awi integrate` detects installed Agent clients and registers AWI with every
supported MCP host in one pass:

```bash
# Preview without modifying configuration.
awi integrate --project-root /path/to/workspace --dry-run --json

# Configure every detected supported client.
awi integrate --project-root /path/to/workspace

# Restrict the operation to selected clients.
awi integrate --client codex,gemini,trae --project-root /path/to/workspace
```

The command is idempotent and reports one status per client:
`configured`, `already_configured`, `would_configure`, `needs_attention`,
`not_installed`, `unsupported`, or `failed`. It currently uses the official MCP
CLI for Codex, Gemini, and Claude Code. For TraeCode, it atomically merges
`<project>/.trae/mcp.json`, which is shared by TraeCode IDE and TraeCode CLI;
project-level MCP must be enabled once in TraeCode settings. Zcode and Kimi Code
are detected but reported as unsupported when their installed version exposes
no stable external stdio MCP registration surface. Gemini workspaces marked
untrusted are reported as `needs_attention` because Gemini suppresses all MCP
servers until the user explicitly trusts the workspace.

By default, AWI registers the current binary as
`awi --index-dir <absolute-path> mcp`, which is self-contained for a local
mutable index. Snapshot-based production deployments must explicitly select
their wrapper with `--server-command`; repeated `--server-arg` options are
available for custom launchers.

## Safety

`workspace_query` accepts only one read-only `SELECT` or `WITH` statement over
explicit inputs beneath registered roots. It enforces timeout, row, and output
byte limits while disabling DuckDB extension loading and external access.

Search and inspection responses are bounded. Sensitive files, generated
directories, oversized content, and symlink escapes are excluded by default.
Default-excluded directories are `.git`, `.hg`, `.svn`, `.awi-index`,
`node_modules`, `target`, `__pycache__`, `.pytest_cache`, `.mypy_cache`,
`.ruff_cache`, `.ipynb_checkpoints`, `.venv`, `.idea`, `.vscode`, and `.cache`,
alongside `.gitignore` and `.awiignore` rules. The `watch` producer applies the
same exclusions to filesystem events, so churn in those directories never wakes
a reconcile. Search root filters accept either an indexed root or an existing
parent scope that contains indexed roots. Structured-query roots remain exact
allowlist entries.

MCP audit logging is optional. When enabled, AWI writes private (`0600`) JSONL
records containing bounded and credential-redacted arguments, duration, outcome,
response bytes, and hit/row counts. The active log rotates at 64 MiB and retains
one previous file.

## Evaluation

The current development evaluation reports:

- Recall@10: 1.0
- Search P95: 6.83 ms
- Stale-result rate: 0
- Real-Agent paired tool-call reduction: 68.4%
- Real-Agent paired search reduction: 54.0%
- Real-Agent paired wall-time reduction: 41.3%

See
[`evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md`](evaluation/two_shot_plus_tongyong_agent_ab_20260920/README.md)
for the protocol, category breakdown, caveats, and reproducibility artifacts.
The evaluation set remains a development candidate pending dual human review;
it is not a frozen release benchmark.

## License

MIT
