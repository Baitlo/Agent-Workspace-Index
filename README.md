# AWI: Agent Workspace Index

AWI is a local-first unified index for code, data, and Agent operational
knowledge. It indexes source code, SQL, documents, logs, JSON/JSONL, CSV/TSV,
Parquet, `AGENTS.md`, Agent Skills, and project-scoped cross-Agent memory through
one bounded CLI and Model Context Protocol (MCP) surface.

One command connects that index to 16 coding-agent harnesses, including Codex,
Claude Code, Gemini CLI, GitHub Copilot CLI, OpenCode, Qwen Code, Cline, Zed,
Amazon Q Developer, and Crush.

## Capabilities

- Tantivy hybrid retrieval over paths, text, symbols, and dataset schemas.
- Tree-sitter symbol extraction for Rust, Python, and Go.
- Embedded, read-only DuckDB queries over explicitly allowlisted files.
- Scope-aware `AGENTS.md` retrieval and structured `SKILL.md` metadata.
- Project-aware memory discovery across Trae, Codex, Zcode, Gemini, and Claude.
- SQLite catalog checks that reject stale search results.
- NFS-safe `notify` and `reconcile` update paths.
- Immutable generation snapshots with atomic daemon activation.
- MCP tools: `workspace_search`, `workspace_inspect`, and `workspace_query`.

See [Agent knowledge indexing](docs/agent-knowledge.md) for discovery, scope,
ranking, deduplication, and safety semantics. See
[Cross-Agent memory indexing](docs/agent-memory.md) for memory sources and the
raw-history boundary.

## Agent-assisted Install

Give your coding agent the
[recommended installation prompt](docs/agent-install-prompt.md), or run the
installer from an AWI checkout:

```bash
bash scripts/install.sh --workspace /absolute/path/to/your/repository
```

The first run builds and installs `awi`, creates a local index outside the
workspace, indexes ancestor `AGENTS.md` files and `SKILL.md` manifests found in
allowlisted Agent directories, indexes curated memory associated with that
workspace, detects installed Agent clients, and registers the AWI MCP server
with each supported client. The operation is idempotent. Pass
`--skip-agent-knowledge` or `--skip-agent-memory` to disable either source
class. Raw chats remain excluded unless `--include-raw-memory` is supplied.
Rust and Cargo are required when building from source.

If Pi is detected, the installer also installs the pinned
`pi-mcp-adapter@2.36.0`, because Pi intentionally has no built-in MCP client.
This is a third-party Pi package; pass `--skip-pi-adapter` to review or install
it separately, or set `AWI_PI_MCP_ADAPTER_SPEC` to select another reviewed
version. Run `scripts/install.sh --help` for custom binary, index, and client
options.

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

# Resolve instructions applicable to a concrete workspace path.
awi --index-dir /tmp/my-awi-index search "build and test rules" \
  --kind agent_instructions --context-path /path/to/workspace/src/lib.rs --json

# Index and search a standalone global Skill manifest.
awi --index-dir /tmp/my-awi-index reconcile ~/.agents/skills/example/SKILL.md --json
awi --index-dir /tmp/my-awi-index search "diagnose deployment failures" \
  --kind agent_skill --json

# Discover curated memory for this project and search it.
awi --index-dir /tmp/my-awi-index memory --project-root /path/to/workspace
awi --index-dir /tmp/my-awi-index search "previous rollout decision" \
  --kind agent_memory --context-path /path/to/workspace --json

# Start the MCP stdio adapter.
awi --index-dir /tmp/my-awi-index mcp

# Persist actual MCP tool calls for later retrieval analysis.
awi --index-dir /tmp/my-awi-index mcp \
  --audit-log /shared/awi/runtime/calls.jsonl
```

Memory uses a separate Tantivy index and is searched only when
`--kind agent_memory` is requested, so adding memory cannot change ordinary
code/data ranking. MCP search uses the compact `compact_v2` shape with a
1,000-character preview, defaults to 5 hits, and compacts requests above 20 to
20. Start with one identifier-rich query instead of parallel near-synonym
searches, and expand only when the first result set lacks evidence. Use
`workspace_inspect` only after selecting a returned path; when the exact path is
already known, read it directly with the host's file tools. Search snippets are
generated from bounded stored source windows, and light directory-diversity
reranking prevents one artifact folder from filling the result set.

### Semantic Retrieval

AWI can add a Harrier GGUF Q8 semantic lane without changing the MCP tool
surface. Document embeddings are computed during `reconcile`, `notify`, or an
initial `semantic-build`; normal searches compute only the query embedding.
SQLite remains authoritative, and semantic candidates whose file generation is
not current are discarded.

Create a dedicated Python environment and enable the sidecar:

```bash
python3 -m venv ~/.cache/awi/semantic-venv
~/.cache/awi/semantic-venv/bin/pip install -r requirements-semantic.txt

export AWI_SEMANTIC_MODEL=/path/to/harrier-oss-v1-270M-Q8_0.gguf
export AWI_SEMANTIC_MODEL_SHA256=fe12f3583dbbb832def4cffeb46c0d0ab49a3288542d5cdbaeb1741315a01b87
export AWI_SEMANTIC_PYTHON="$HOME/.cache/awi/semantic-venv/bin/python"
export AWI_SEMANTIC_THREADS=16
export AWI_SEMANTIC_EMBED_WORKERS=1
```

For an existing catalog, precompute every eligible file once:

```bash
awi --index-dir /tmp/awi-writer semantic-build \
  --publish-dir /shared/awi-publication --json
```

An interrupted bulk build can reuse complete file generations already committed
to LanceDB. If workspace content changed during the failed attempt, reconcile
the catalog without publishing first, then resume:

```bash
env -u AWI_SEMANTIC_MODEL \
  awi --index-dir /tmp/awi-writer reconcile /path/to/workspace --json
awi --index-dir /tmp/awi-writer semantic-build --resume \
  --publish-dir /shared/awi-publication --json
```

Resume mode still re-hashes every source against SQLite and reuses only exact
`(file_id, generation)` pairs. Seal performs the same full coverage check and
prunes stale pairs before publication.

The build uses tokenizer-aware, structure-sensitive chunks capped at 480 model
tokens, keeps at most four chunks per file, applies the Harrier query
instruction only to queries, and L2-normalizes embeddings. Source, text,
semi-structured, and tabular content are eligible; sensitive or metadata-only
files and Agent memory are excluded. A nonzero eligible file whose extracted
payload is empty receives a path-and-type header vector so build and publication
coverage stay identical; zero-byte files remain excluded.

Bulk builders can set `AWI_SEMANTIC_EMBED_WORKERS` above one. Extra GGUF model
instances are loaded lazily for document embedding only; query serving keeps a
single model. AWI divides `AWI_SEMANTIC_THREADS` across those workers, so set it
to the total CPU quota available to the sidecar.

Subsequent producer cycles update only changed files. Before publication, AWI
requires LanceDB coverage for every current eligible `(file_id, generation)`.
The semantic database and its manifest are copied into the same immutable
snapshot as SQLite and Tantivy. Readers reject a mismatched generation and
fall back to lexical search when the sidecar is unavailable. Unset
`AWI_SEMANTIC_MODEL` to disable the semantic lane.

Query-time semantic retrieval runs concurrently with the lexical lanes. Vector
overfetch is bounded by the configured maximum chunks per file, preserving
file-level Top-K coverage without returning redundant chunk candidates.
Persistent readers run one full hybrid warmup before serving queries. The sidecar
uses a Linux parent-death signal so stopping the reader also releases the model.

Sealed generations use an IVF_FLAT index and probe every partition. This keeps
the original normalized Q8 vectors and exact Top-K ordering while avoiding the
recall loss of product quantization at AWI's current corpus size.

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

Snapshot download and checksum validation run on a background refresh worker.
Requests continue against the last valid generation while a new snapshot is
materialized, avoiding NFS refresh pauses on the query path.
An individual client disconnect or write failure is logged without terminating
the shared daemon.

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
awi integrate --client codex,gemini,opencode,qwen,cline,zed,amazon-q,crush \
  --project-root /path/to/workspace
```

The command is idempotent and reports one status per client:
`configured`, `already_configured`, `would_configure`, `needs_attention`,
`not_installed`, `unsupported`, or `failed`. It currently uses the official MCP
CLI for Codex, Gemini, and Claude Code, and native JSON configuration for the
other clients:

| Client | Registration target |
|---|---|
| [GitHub Copilot CLI](https://docs.github.com/en/copilot/how-tos/copilot-cli/customize-copilot/add-mcp-servers) | `~/.copilot/mcp-config.json` |
| TraeCode | `<project>/.trae/mcp.json` |
| Zcode | `~/.zcode/cli/config.json` at `mcp.servers` |
| Kimi Code | `$KIMI_CODE_HOME/mcp.json`, defaulting to `~/.kimi-code/mcp.json` |
| [OpenCode](https://opencode.ai/docs/en/mcp-servers/) | `$OPENCODE_CONFIG`, or `${XDG_CONFIG_HOME:-~/.config}/opencode/opencode.json` |
| [Pi](https://github.com/badlogic/pi-mono/tree/main/packages/coding-agent) | `$PI_CODING_AGENT_DIR/mcp.json` through [`pi-mcp-adapter`](https://pi.dev/packages/pi-mcp-adapter) |
| [Cursor](https://cursor.com/help/customization/mcp) | `~/.cursor/mcp.json` |
| [Windsurf](https://docs.windsurf.com/windsurf/cascade/mcp) | `~/.codeium/windsurf/mcp_config.json` |
| [Qwen Code](https://qwenlm.github.io/qwen-code-docs/en/users/features/mcp/) | `~/.qwen/settings.json` |
| [Cline CLI](https://docs.cline.bot/mcp/mcp-overview) | `~/.cline/data/settings/cline_mcp_settings.json` (current CLI); `~/.cline/mcp.json` (legacy IDE-only fallback) |
| [Zed](https://zed.dev/docs/ai/mcp) | `${XDG_CONFIG_HOME:-~/.config}/zed/settings.json` at `context_servers` |
| [Amazon Q Developer](https://docs.aws.amazon.com/amazonq/latest/qdeveloper-ug/command-line-mcp-configuration.html) | `~/.aws/amazonq/mcp.json` |
| [Crush](https://www.mintlify.com/charmbracelet/crush/configuration/mcp) | `${XDG_CONFIG_HOME:-~/.config}/crush/crush.json` |

Plugins are optional packaging for clients with native MCP support. Pi is the
exception: its core deliberately omits MCP, so an extension is required. New
sessions load the generated user-level entries automatically. TraeCode
project-level MCP must be enabled once in settings. Gemini workspaces marked
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

Search and inspection responses are bounded. Agent knowledge discovery is
limited to ancestor `AGENTS.md` files and `SKILL.md` manifests under known
per-client directories. Memory discovery is limited to curated files mapped to
the selected project; raw histories require explicit opt-in. AWI never indexes
an entire home directory. Sensitive files, generated directories, oversized
content, and symlink escapes are excluded by default.
Default-excluded directories are `.git`, `.hg`, `.svn`, `.awi-index`,
`node_modules`, `target`, `__pycache__`, `.pytest_cache`, `.mypy_cache`,
`.ruff_cache`, `.ipynb_checkpoints`, `.venv`, `.idea`, `.vscode`, and `.cache`,
plus Agent-created `.codex-work` and `.worktrees`, alongside `.gitignore` and
`.awiignore` rules. The `watch` producer applies the same exclusions to
filesystem events, so churn in those directories never wakes a reconcile.
Search root filters accept either an indexed root or an existing parent scope
that contains indexed roots. Structured-query roots remain exact allowlist
entries.

MCP audit logging is optional. When enabled, AWI writes private (`0600`) JSONL
records containing bounded and credential-redacted arguments, caller process,
duration, outcome, error detail, response bytes, text/structured payload bytes,
hit/row counts, and separate preview-truncation and limit-compaction flags.
Schema v3 retains the aggregate `result_truncated` field for compatibility. The
active log rotates at 64 MiB and retains one previous file.

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
