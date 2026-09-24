# Session Adoption Funnel

AWI measures adoption only over project sessions that actually require
retrieval. The old metric, sessions with an AWI call divided by all project
sessions, remains available as `raw_session_adoption` but is not the primary
adoption metric.

## Denominator

A session is `eligible` when completing the user request requires discovering
an unknown code symbol, file, configuration, SQL artifact, dataset, instruction,
Skill, or prior project evidence. Sessions that only edit an exact path already
provided by the user, run a known command, or perform non-retrieval work are
`ineligible`. Ambiguous sessions are `unknown` and are excluded from the primary
denominator while reducing reported label coverage.

Store one JSON object per session, following
[`session-observation.schema.json`](session-observation.schema.json). Do not
infer eligibility from the presence of an AWI call: that makes the numerator
define its own denominator. Heuristic labels may be used for operational
triage, but formal reports require independent review.

## Stages

For every eligible session, record:

1. `tool_visible`: the client exposed AWI or an AWI discovery entry.
2. `namespace_loaded`: `workspace_search` became callable by the model.
3. `search`: the session issued an actual AWI search call.
4. `inspect`: the session issued an AWI inspect linked to a selected result.
5. `evidence_adopted`: the answer or a subsequent action used AWI evidence.

Use `null`, not `false`, when the trace cannot establish a stage. A later true
stage requires all mandatory earlier stages to be true. `inspect` is diagnostic,
not a prerequisite for evidence adoption: a bounded search preview can be
sufficient evidence, so the report also counts `adopted_without_inspect`.

For Codex, count actual `response_item` function calls, not tool definitions or
instruction text. A matching `tool_search_output` establishes namespace loading.
For Zcode, use `request.toolNames` and `response.toolCalls`; use
`mcp.tools.registered` only for visibility, not as proof that the model received
the namespace.

## Report

```bash
python3 scripts/audit-session-adoption.py \
  observations.codex.jsonl observations.zcode.jsonl \
  --output adoption-report.json
```

The report contains:

- `raw_session_adoption`: searched sessions divided by all sessions;
- `eligible_session_adoption`: searched eligible sessions divided by all
  eligible sessions;
- per-stage reached, not-reached, unknown, and conditional conversion counts;
- the same breakdown by client, project, and project/client pair;
- review blockers and a report status.

The stage denominators are:

| Stage | Denominator |
|---|---|
| tool visible | eligible sessions |
| namespace loaded | eligible sessions where the tool was visible |
| search | eligible sessions where the namespace was loaded |
| inspect | eligible sessions that searched |
| evidence adopted | eligible sessions that searched |

## Review Gate

Every eligibility label must have an `approved` review with at least two
distinct reviewers. Every searched eligible session must also have a known,
dual-reviewed `evidence_adopted` label. Any stage that should be observable for
an eligible session must be non-null. Until these conditions hold, the report
sets `report_status=development_only` and lists blocking reasons.

Example observation:

```json
{"schema_version":1,"session_id":"session-001","client":"codex","project_root":"/workspace","eligibility":"eligible","eligibility_reason":"locate an unknown symbol implementation","eligibility_review":{"status":"approved","reviewers":["reviewer-a","reviewer-b"]},"stages":{"tool_visible":true,"namespace_loaded":true,"search":true,"inspect":false,"evidence_adopted":true},"evidence_review":{"status":"approved","reviewers":["reviewer-a","reviewer-b"]}}
```
