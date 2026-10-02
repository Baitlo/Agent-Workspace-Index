# Task outcome review

`audit-call-outcomes.py` schema 2 keeps the existing three-action follow-up proxy
and adds private user-turn packets. Each packet includes the question, ordered
calls, search results and IDs, final answer when explicitly marked, path reuse
candidates, exact-repeat counts and elapsed user-to-final time. Only Codex
response-item traces are currently supported. A user turn is an observable
boundary, not necessarily a complete multi-turn task.

```bash
python3 scripts/audit-call-outcomes.py \
  --audit /private/mcp-calls.jsonl \
  --codex-sessions /private/codex/sessions/2026/10/02 \
  --project-root /absolute/workspace --since 2026-10-02 \
  --output /private/task-audit.json \
  --review-template /private/review-template.json
```

Give independent reviewers the relevant task packet and source evidence. Each
reviewer writes a separate JSON document with `split: "dev"` and `reviews`:

| Field | Meaning |
|---|---|
| `task_id` | Exact packet identity |
| `reviewer_id` | Nonempty reviewer identity; normalized for duplicate detection |
| `independent` | Explicit independence attestation, required true |
| `retrieval_eligible` | Whether this task required discovering unknown evidence |
| `answer_correct` | Whether the final answer is correct |
| `evidence_correct` | Whether the adopted evidence supports the answer |
| `evidence_adopted` | Whether AWI evidence was used in the answer or task outcome |
| `adopted_search_keys` | Exact search keys within this turn; required for adoption |
| `supporting_evidence` | Specific source/answer lines and rationale |

The four judgment fields accept true, false, or null. A path appearing in another
tool or a final answer is a review candidate, not an automatic positive label.
Read sufficient source evidence to distinguish correct, irrelevant and stale
copies. A useful preview can establish adoption without an inspect call.

Rerun with `--reviews /private/reviewer-a.json --reviews /private/reviewer-b.json`.
Duplicate reviewer/task records, foreign tasks/search keys and non-dev inputs
are rejected. Only at least two agreeing reviews contribute labels; disagreements
remain disputed, single reviews remain provisional, and missing labels remain
unknown. Reviewer independence is attested, not technically authenticated.

The aggregate reports correctness among reviewed eligible tasks, correct-evidence
adoption among known outcomes, annotation coverage, and adoption bounds accounting
for unknown eligible outcomes. The visibility/loading funnel preserves unknown
telemetry. Never present the known-only rate without coverage, or the observed
turn wall time as a causal retrieval speedup. Task packets with no AWI searches
remain available for eligibility review rather than being removed from the
potential denominator. Subagents without user messages can remain unlinked.

Reports and templates are mode 0600. They may contain private questions, paths,
and source excerpts; keep them outside the repository. Use a new output path
for each run so previously reviewed evidence is retained.
