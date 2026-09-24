# Qianchuan Retrieval Evaluation (2026-09-19)

This directory contains a development-only retrieval candidate for AWI. It covers:

- Python implementation and analysis code for `two_shot` and `tongyong`.
- Hive SQL for targeted posterior refresh and Tongyong dataset construction.
- Local and canonical NFS experiment summaries.
- JSONL/TSV datasets discoverable through indexed schema fields and queryable with `workspace_query`.

## Safety state

`dev.json` is marked `candidate_pending_dual_review`. It may only be evaluated with `awi benchmark --allow-draft`. It is not a frozen test set and cannot open the release gate. The evaluator rejects every `split=frozen` file and requires two distinct reviewers before an approved development set can open the review sub-gate.

Do not create or consume a frozen split, or report formal precision/recall,
Inspect@K, or MRR, until the independent reviews are complete, disagreements
are adjudicated, and the development metrics satisfy all gates. Metrics from a
candidate set must be labeled development-only diagnostics.

## Gates

- Recall@10: at least 0.95, computed as retrieved relevant paths divided by all relevant paths per case.
- Search P95: at most 200 ms.
- Stale result rate: exactly 0.
- Agent tool-call reduction: at least 50% against reviewed no-AWI baselines.
- Review: two distinct reviewers and `review_status=approved`.

The current `baseline_tool_calls` values are candidate estimates. Reviewers must replace or confirm them from equivalent no-AWI traces before approval.

## Candidate run

Build all four corpus roots into one index, then run:

```bash
awi --index-dir <index> benchmark \
  --gold evaluation/two_shot_plus_tongyong_qianchuan_retrieval_20260919/dev.json \
  --output evaluation/two_shot_plus_tongyong_qianchuan_retrieval_20260919/candidate_report.json \
  --allow-draft
```

For structured-query acceptance, query an explicit file under one of the allowlisted roots. Example:

```bash
awi query \
  --root /mnt/bn/baiweikang/qianchuan_distill/workspace_assets/datasets/legacy_four_model_inputs \
  --file /mnt/bn/baiweikang/qianchuan_distill/workspace_assets/datasets/legacy_four_model_inputs/judge300_ubx.jsonl \
  --sql 'SELECT source_type, count(*) AS rows FROM data_0 GROUP BY source_type ORDER BY rows DESC' \
  --max-rows 20 --max-bytes 1048576 --timeout-ms 10000
```
