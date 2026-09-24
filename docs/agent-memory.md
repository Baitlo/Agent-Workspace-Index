# Cross-Agent Memory Indexing

AWI can discover project-relevant memory from supported coding Agents and make
it searchable through the existing `workspace_search` and `workspace_inspect`
tools. It does not add a separate MCP tool.

## Usage

```bash
# Curated memory and summaries only.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace

# Explicitly include matching raw chat/session history.
awi --index-dir /tmp/my-awi-index memory \
  --project-root /absolute/path/to/workspace \
  --include-raw

# Search only memory associated with the current project.
awi --index-dir /tmp/my-awi-index search "previous deployment decision" \
  --kind agent_memory \
  --context-path /absolute/path/to/workspace \
  --json
```

The installer runs the curated form by default. Use
`--skip-agent-memory` to disable it or `--include-raw-memory` to opt in to raw
history.

## Sources

Discovery is constrained to the selected canonical project:

| Agent | Default curated sources | Raw opt-in |
|---|---|---|
| Trae | user profile, matching project memory, topic summaries, session summaries | none beyond those summaries |
| Codex | `MEMORY.md`, `memory_summary.md`, matching rollout summaries | linked raw rollout JSONL and `raw_memories.md` |
| Zcode | memory root mapped to the project by Zcode runtime metadata | matching rollout/agent files |
| Gemini CLI | none | mapped project history and chat directories |
| Claude Code | matching project memory directory | session files that identify the project |

Argos/SRE session directories are intentionally excluded because they require
the Argos diagnostic workflow rather than bulk file indexing. AWI never scans
all of the home directory.

## Metadata And Ranking

Every memory hit includes:

- source Agent;
- layer: `user_profile`, `project_summary`, `topic_summary`,
  `session_summary`, `memory_note`, or `raw_history`;
- optional YAML frontmatter `name` and `description`;
- canonical workspace root and provider project key when available;
- session ID when it can be derived;
- observation time and raw-history flag.

When `context_path` or a project root filter is supplied, memory from another
project is rejected. Exact-project memory ranks ahead of global memory. Exact
filename and frontmatter `name` matches receive the strongest metadata boosts;
description overlap provides a smaller boost. Identifier-shaped queries
demote broad `MEMORY.md`/project summaries unless the summary itself exactly
matches the entity. The normal layer order remains unchanged for broad queries,
curated summaries rank ahead of raw history, and recency is only a small
tie-breaker. Content-identical copies from different Agents are collapsed after
ranking. Memory retrieval is activated with `--kind agent_memory`. Memory
documents use a dedicated Tantivy index so their vocabulary cannot change
ordinary code/data IDF statistics or ranking.

## Parsing And Safety

Markdown memory is indexed as bounded text. JSON/JSONL memory is normalized to
human-relevant fields such as intent, actions, outcome, learned facts, role,
message, and content; transport IDs and internal digest metadata are omitted
from the search document. Memory JSONL is not sent to DuckDB profiling.

Normal size, UTF-8, ignore, and sensitive-filename checks still apply.
High-confidence credentials or private keys cause the entire memory file to be
metadata-only. Raw history is disabled unless explicitly requested, and
oversized raw files remain metadata-only under the configured content limit.

`awi memory` persists the canonical project root and raw-history policy. Every
producer cycle rediscovers that project's Agent sources before publishing, so
new provider project directories appear without reinstalling AWI. Previously
registered roots are also reconciled when they disappear, preventing deleted
memory from remaining searchable. Appended summary JSONL is refreshed on the
next periodic reconcile; append-heavy raw files do not trigger per-write
snapshot rebuilds.

`awi status --json` exposes a `memory` object with registered project/source
counts, active files, the newest memory generation and relative generation lag,
filesystem `stale_files`/`missing_files`, maximum source lag, and oldest source
age. Filesystem stale and missing counts are the authoritative freshness
signals; a low generation lag alone does not prove that external memory is
current.

## Development Validation

An isolated Bona run discovered 35 project-relevant roots and indexed 489
curated files from Codex, Trae, and Zcode with zero extraction failures; Gemini
raw chats remained excluded. The existing 16-case code/data retrieval set stayed
bit-for-bit stable at Recall@10 `1.0`, MRR `0.6006`, nDCG@10 `0.6950`, and stale
rate `0`. Release-build P95 was `24.06 ms`. This set is still
`candidate_pending_dual_review`, not a frozen release gate. Do not report
formal precision/recall, Inspect@K, or MRR until two distinct reviewers have
independently labeled the query set and adjudicated disagreements.
