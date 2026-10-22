# AWI Real-Agent A/B Evaluation (2026-09-20)

## Status

This is a development candidate, not a release-approved benchmark.
`tasks.dev.json` remains `split=dev` and
`review_status=candidate_pending_dual_review`, with current SHA256
`0e6dd21209d2115ba2b61aad2c6ab68520c9b6281142bb4b695f323a6ad1fa22`.
No frozen split was created or consumed. The runner rejects frozen tasks and
requires `--allow-draft` for candidate runs; use that flag only for pilots.
Release remains blocked until two fresh independent reviewers approve the
current questions, expected paths, answer checks, and raw traces.

## Candidate Protocol

- Model: `llmbox-gpt-5.6-sol`, medium reasoning, ephemeral sessions.
- 12 tasks across code, SQL, experiment-result, and dataset-query categories.
- Three repetitions with shuffled task order and alternating arm order: 72 trials.
- Both arms used isolated `CODEX_HOME`, `TMPDIR`, empty working directories, a
  read-only sandbox, identical questions, timeout, and output schema.
- Baseline used ordinary read-only shell retrieval. AWI used only
  `workspace_search`, `workspace_inspect`, and `workspace_query` over MCP.
- The current runner records MCP arguments and returned hit paths so shell and
  AWI scope violations are checked independently.
- Raw traces and results are retained under
  `validation/agent-ab-final-20261022/output/`.

## Candidate Results

| Metric | Baseline | AWI | Reduction |
|---|---:|---:|---:|
| Correct | 36/36 | 36/36 | equal |
| Agent tool calls | 144 | 57 | 60.4% |
| Discovery searches | 108 | 36 | 66.7% |
| Wall time | 647.78 s | 485.18 s | 25.1% |

The tool-call gate passes. The wall-time gate fails: the 30% target requires
AWI to finish within 453.44 s, so this run is 31.74 s over the ceiling.
There were no policy violations, scope violations, timeouts, or incorrect
answers. The wall delta mean 95% bootstrap interval is 2.08 to 6.87 seconds.

## Paired Results By Category

| Category | Pairs | Tool-call reduction | Wall-time reduction |
|---|---:|---:|---:|
| Code | 12 | 62.8% | 25.7% |
| Dataset query | 6 | 73.5% | 19.0% |
| Experiment result | 9 | 43.3% | 8.7% |
| SQL | 9 | 59.5% | 41.1% |

`experiment_result` is the blocking category. In particular,
`result-v47-audit` took 72.62 s with AWI versus 52.78 s with the baseline after
one failed DuckDB query and one wrong `_current` file inspection. The latest
task and prompt revisions remove four answer hints, bind the SQL join key to
its value, prefer JSON-object inspection over array functions, and prohibit
retrying the same failed query. These changes require a fresh pilot and full
72-run rerun.

## Iteration Record

- `agent-ab-final-20261022`: 72 runs, 36/36 correct pairs, tool calls -60.4%,
  wall time -25.1%; rejected because the wall gate and experiment-result
  category remain below target.
- Reviewers L and M independently rejected the run and are retained under
  `validation/reviews/`; reviewer K records the earlier 48-run rejection.
- `full-v2`: both arms 24/24; tool calls -26.6%, wall time -6.0%. Exposed
  missing TSV classification and insufficient preview/tool guidance.
- `full-v3`: intentionally stopped after 19 results when JSONL was repeatedly
  misclassified by the Agent due an unclear MCP kind contract.
- `full-v4`: both arms 24/24; tool calls -55.2%, wall time -24.6%. Correct but
  below the 30% wall-time gate.
- `full-v5`: paired tool calls -64.8%, wall time -33.0%; rejected because AWI
  omitted two exact identifiers in one answer.
- `full-v6`: AWI 24/24; exposed an over-strict answer-token check on a baseline
  answer that correctly reported the requested values.
- `full-v7`: final candidate after fixing that evaluator defect; metrics above.

## Artifacts

- `final-20261022/results.jsonl`: all 72 candidate results.
- `final-20261022/summary.json`: arm and paired aggregates.
- `final-20261022/category_summary.json`: per-category reductions.
- `final-20261022/runner.log` and `runner-rep3.log`: split runner coverage.
- `test_run_agent_ab.py`: task guards, answer binding, and AWI scope tests.
- Shared review and raw-trace evidence:
  `/mnt/bn/baiweikang/qianchuan_distill/awi/runtime/production-20261022/validation/`.

## Worker snapshot preflight

For the 2026-10-22 production rerun, the runner accepts `--model-catalog` and
allows either a local `catalog.sqlite3` or a snapshot-reader `snapshot-cache`.
When a CPU worker has an older `/home/tiger` view, run both arms inside a mount
namespace over a current master-home mirror. Otherwise absolute-path freshness
checks produce false stale hits even though catalog generations match.
