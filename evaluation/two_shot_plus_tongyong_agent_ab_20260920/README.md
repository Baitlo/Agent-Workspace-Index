# AWI Real-Agent A/B Evaluation (2026-09-20)

## Status

This is a development candidate, not a release-approved benchmark.
`tasks.dev.json` remains `split=dev` and
`review_status=candidate_pending_dual_review`. No frozen split was created or
consumed. Release remains blocked until two independent reviewers approve the
tasks, expected paths, answer checks, and baseline traces.

## Final Protocol

- Model: `llmbox-gpt-5.6-sol`, medium reasoning, ephemeral sessions.
- 12 tasks across code, SQL, experiment-result, and dataset-query categories.
- Two repetitions with shuffled task order and alternating arm order: 48 trials.
- Both arms used isolated `CODEX_HOME`, `TMPDIR`, empty working directories, a
  read-only sandbox, identical questions, timeout, and output schema.
- Baseline used ordinary read-only shell retrieval. AWI used only
  `workspace_search`, `workspace_inspect`, and `workspace_query` over MCP.
- Raw traces and results are retained under the shared `full-v7` directory.

## Final Results

Primary efficiency results use the 23 pairs where both arms were correct. This
avoids crediting AWI for the one baseline failure.

| Metric | Baseline | AWI | Reduction |
|---|---:|---:|---:|
| Agent tool calls | 98 | 31 | 68.4% |
| Discovery searches | 50 | 23 | 54.0% |
| Wall time | 488.88 s | 287.00 s | 41.3% |

- AWI was faster in 21/23 both-correct pairs; baseline was faster in 2/23.
- Median paired delta: 3 tool calls, 1 search, and 8.05 seconds saved.
- Mean wall-time delta 95% bootstrap interval: +5.55 to +12.26 seconds.
- Mean tool-call delta 95% bootstrap interval: +2.22 to +3.65 calls.

All-run results include all 24 pairs:

| Metric | Baseline | AWI | Reduction |
|---|---:|---:|---:|
| Correct | 23/24 | 24/24 | AWI +1 task |
| Agent tool calls | 106 | 32 | 69.8% |
| Discovery searches | 56 | 24 | 57.1% |
| Wall time | 543.20 s | 297.44 s | 45.2% |
| Mean wall time | 22.63 s | 12.39 s | 45.2% |
| P95 wall time | 46.85 s | 17.85 s | 61.9% |
| Input tokens | 3,455,081 | 1,266,638 | 63.3% |
| Cached input tokens | 2,745,344 | 886,784 | 67.7% |
| Output tokens | 24,843 | 11,873 | 52.2% |

The baseline failure was `r01-code-ziti-root-baseline`: after eight tool calls it
selected the wrong evaluation script/model root. AWI answered that task correctly.
There were no policy violations, timeouts, or nonzero exits in either arm.

## Paired Results By Category

| Category | Pairs | Tool-call reduction | Search reduction | Wall-time reduction |
|---|---:|---:|---:|---:|
| Code | 7 | 66.7% | 41.7% | 26.0% |
| Dataset query | 4 | 68.4% | 63.6% | 35.1% |
| Experiment result | 6 | 68.0% | 40.0% | 46.6% |
| SQL | 6 | 69.7% | 64.7% | 50.1% |

Code wall time is the only category below the 30% target. The aggregate paired
wall-time gate passes. Treat the bootstrap interval as uncertainty evidence, not
as a formal independent-sample significance claim, because tasks repeat.

## Retrieval And Build Evidence

Candidate-v3 was rebuilt from scratch on MLX CPU worker `4288907` and published
as immutable generation 4.

- 1,278 active files, 12,284 symbols, 309 datasets, 0 indexing failures.
- Build elapsed time: 26 seconds; sampled peak RSS: 509,896 KiB.
- Snapshot size: 14,916,222 bytes.
- Recall@10: 1.0; MRR: 0.6534; nDCG@10: 0.7360.
- Search P50/P95: 3.42/6.83 ms; stale-result rate: 0.

The offline release gate remains false only because dual human review is pending.

## Iteration Record

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

- `final_summary.json`: final arm and paired aggregates.
- `final_category_arms.json`: all-run category totals.
- `final_category_paired.json`: both-correct category comparisons.
- `candidate_v3_build_metrics.json`: index build and offline retrieval metrics.
- `retrieval_report.json`: complete 16-case offline retrieval report.
- Shared raw traces: `/mnt/bn/baiweikang/qianchuan_distill/awi/agent_eval/two_shot_plus_tongyong_agent_ab_20260920/full-v7`.
- Immutable snapshot: `/mnt/bn/baiweikang/qianchuan_distill/awi/publications/two_shot_plus_tongyong_qianchuan_retrieval_20260919/candidate-v3`.
