# Gold Set Adjudication

This file records reviewer disagreements and the evidence used to resolve them.
Independent reviewers must complete their own review before reading it.

## result-001: complete audit

The original label pointed to
`Qianchuan102_data_process/logs/v47_multi_output_audit_current.json`, which has
`status=incomplete` and null retention values. Both first-round reviewers
rejected it. The label now points to the completed audit:

- `status=complete`
- 12 `(scene,date)` retention entries
- non-null `P_t` and `N_t` final-row and retention-rate values

## Baseline command counts

The blind shell runner returned candidate per-case counts that did not equal
the number of commands it enumerated. The raw output is preserved as
`baseline-shell-20261022.raw.json`. For the reviewed set, a tool call is defined
as one command listed for the matching case. This produces 59 calls across 16
cases and is recorded explicitly in both the normalized baseline file and
`dev.json`.

## code-004: three-model run

The target script's docstring retained a stale "five models" phrase, but its
actual `MODEL_SPECS` contains three models and its contract identifies the
three-model Ziti History anchor run. The query and evidence now describe three
models.

## result-002: reachable-service scope

The corrected suite contains four reachable services across three scenes. The
compact summary has eight service/mode rows with three requests each; the
source matrix has 24 rows. Both are relevant and the evidence states their
actual granularity.

## Dataset copies

QC102 and UBX `judge300` files have byte-identical local and NFS copies. To keep
one content item from appearing twice as separate positives, both dataset cases
now explicitly request the canonical NFS copy.

## dataset-004: full run scope

The selected c128 retry file is the effective full2983 run because the sibling
`c128_full2983` directory is empty. The raw file contains 5966 attempts across
two services; the co-listed manifest has a separate 2983-request summary for
each service. The evidence reports both service summaries.

## dataset-001: sparse optional field

`selected_by` is an optional field: it is populated for 558 of 3053 rows and
null for 2495. The case asks whether the schema supports the four named fields;
it does not claim that every row has a non-null `selected_by` value.

## Final independent review

Two fresh reviewers evaluated the corrected set without seeing prior reviews
or this adjudication document:

- `reviewer-f-20261022`: 16 approve, overall approved
- `reviewer-g-20261022`: 16 approve, overall approved

Non-blocking caveats retained in their reports:

- `code-004` has a stale "five models" docstring, but its executable
  `MODEL_SPECS` contains three models.
- The AIGC and Ziti refresh SQL files share the semi-join treatment but differ
  in source-query bodies and output paths.
- `result-002`'s compact summary aggregates three scenes by service/mode; the
  detailed matrix exposes both modes explicitly.
- `dataset-001.selected_by` is sparse.
- The normalized baseline count intentionally measures one call per listed
  command; the runner's original estimates remain in the raw file.
