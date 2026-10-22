# Qianchuan Retrieval Evaluation (2026-09-19)

This directory contains an independently reviewed development-only retrieval
set for AWI. It covers:

- Python implementation and analysis code for `two_shot` and `tongyong`.
- Hive SQL for targeted posterior refresh and Tongyong dataset construction.
- Local and canonical NFS experiment summaries.
- JSONL/TSV datasets discoverable through indexed schema fields and queryable with `workspace_query`.

## Safety state

`dev.json` is marked `review_status=approved` with two distinct reviewers. It
remains `split=dev`; no frozen split was created. The evaluator rejects every
`split=frozen` file, and an approved set without two distinct reviewers fails
closed.

The independent reviews and adjudication are complete. Metrics from this
development set are release-gate evidence only when the full server objective
also passes on a completed collection.

## Gates

- Recall@10: at least 0.95, computed as retrieved relevant paths divided by all relevant paths per case.
- Search P95: at most 200 ms.
- Stale result rate: exactly 0.
- Agent tool-call reduction: at least 50% against reviewed no-AWI baselines.
- Review: two distinct reviewers and `review_status=approved`.

The `baseline_tool_calls` values come from the independent read-only shell run
stored in `baseline-shell-20261022.json`. Its `runner_id` is
`baseline-shell-20261022`. The reviewed count is exactly one tool call per
command listed for the matching case; those 16 counts (59 total) are copied
into `dev.json`. The normalized JSON SHA256 is
`65c90cce37f7f0a31df44b30cbbf4f24e89815db1a01e5925d89fdb1cbea1eb0`.
The runner's original output is preserved as
`baseline-shell-20261022.raw.json` with SHA256
`67160105f0a05a8991cfff6121d7e7cc336ea9cc770e92559024df7161315690`.
Reviewer F and Reviewer G independently verified the normalized counts,
commands, selected paths, and evidence before approval.

## Reviewed run

Build all four corpus roots into one index, then run:

```bash
awi --index-dir <index> benchmark \
  --gold evaluation/two_shot_plus_tongyong_qianchuan_retrieval_20260919/dev.json \
  --output evaluation/two_shot_plus_tongyong_qianchuan_retrieval_20260919/benchmark-approved.json
```

For structured-query acceptance, query an explicit file under one of the allowlisted roots. Example:

```bash
awi query \
  --root /mnt/bn/baiweikang/qianchuan_distill/workspace_assets/datasets/legacy_four_model_inputs \
  --file /mnt/bn/baiweikang/qianchuan_distill/workspace_assets/datasets/legacy_four_model_inputs/judge300_ubx.jsonl \
  --sql 'SELECT source_type, count(*) AS rows FROM data_0 GROUP BY source_type ORDER BY rows DESC' \
  --max-rows 20 --max-bytes 1048576 --timeout-ms 10000
```
