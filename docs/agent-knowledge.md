# Agent Knowledge Indexing

AWI indexes Agent operating instructions alongside source code and data without
adding another MCP tool.

## Document Types

| File | AWI kind | Structured metadata |
|---|---|---|
| `AGENTS.md` | `agent_instructions` | scope directory, precedence depth, headings, local references |
| `SKILL.md` | `agent_skill` | YAML `name` and `description`, headings, local references |

The complete bounded Markdown text remains searchable. `workspace_inspect`
returns the structured metadata together with the normal file metadata and
content excerpt.

## Discovery

A normal workspace reconcile indexes `AGENTS.md` and `SKILL.md` files beneath
that workspace, subject to `.gitignore`, `.awiignore`, and AWI's default
exclusions.

The first-run installer additionally discovers:

- `AGENTS.md` files in ancestor directories above the selected workspace;
- `SKILL.md` manifests beneath known project and user directories for supported
  Agent clients.

Each external document is registered as a single-file root. AWI does not index
an entire home directory or the full contents of global Skill packages.
`--skip-agent-knowledge` disables this additional discovery.

Local Markdown links from an Agent document are recorded only when their target
exists and remains inside the document root. Referenced files are searchable
when they are already covered by a workspace root; otherwise the reference path
is returned as metadata for explicit inspection with another file tool.

## Scope And Ranking

Pass `context_path` to `workspace_search` when resolving repository
instructions:

```bash
awi search "build and test rules" \
  --kind agent_instructions \
  --context-path /workspace/service/src/main.rs \
  --json
```

AWI excludes `AGENTS.md` files whose scope is not an ancestor of the context
path. Applicable files are ranked by scope depth so the nearest instructions
come first. Content-identical Agent documents are collapsed after this ranking,
which suppresses copied Skill and worktree duplicates without losing the
nearest applicable instruction.

Use `--kind agent_skill` for Skill-focused retrieval. Agent documents are kept
out of ordinary code/data searches; the dedicated Agent lane is activated by an
Agent kind filter or `context_path`.

## Safety

Agent documents remain subject to normal size, UTF-8, symlink, and sensitive
filename checks. High-confidence private-key and provider-token patterns in
their content cause a metadata-only result with
`metadata_only_sensitive_content`; the content, preview, and parsed Agent
metadata are not stored.

Malformed Skill frontmatter is recorded as an extraction failure, while the
new Markdown content still replaces any older indexed text. This avoids serving
stale content when only structured metadata parsing fails.

## Development Validation

An isolated install over the current Bona environment discovered 136 canonical
Agent documents with zero extraction failures. Targeted smoke queries selected
`wukong-dag-failure-debugger` first for a Wukong DAG/LogID failure query and the
Bona `AGENTS.md` for an AWI source context.

On the existing 16-case development retrieval set, adding those Agent documents
kept Recall@10 at `1.0` and stale-result rate at `0`; MRR changed from `0.5975`
to `0.6027`, nDCG@10 from `0.6922` to `0.6969`, and release-build P95 from
`8.07 ms` to `25.88 ms`. The latency remains below the `150 ms` hybrid-search
target. This development set remains pending dual review and is not a frozen
release benchmark.
