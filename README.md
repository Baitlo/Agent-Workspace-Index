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

For Codex, `scripts/awi-mcp-snapshot-wrapper.sh` starts or reuses one local
snapshot daemon before launching the stdio adapter. Configure
`AWI_SNAPSHOT_SOURCE` and `AWI_MCP_AUDIT_LOG`, then register the wrapper as a
global MCP server.

## Safety

`workspace_query` accepts only one read-only `SELECT` or `WITH` statement over
explicit inputs beneath registered roots. It enforces timeout, row, and output
byte limits while disabling DuckDB extension loading and external access.

Search and inspection responses are bounded. Sensitive files, generated
directories, oversized content, and symlink escapes are excluded by default.
Search root filters accept either an indexed root or an existing parent scope
that contains indexed roots. Structured-query roots remain exact allowlist entries.

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
