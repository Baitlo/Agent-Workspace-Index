# AWI production restoration and release evidence — 2026-10-22

Status: in progress. This file records authoritative evidence as the clean
production generation is built; it must not be read as a completed release gate
until the checklist at the end is complete.

## Code baseline

- Code commit at the start of deployment: `21da0b4`; current main:
  `166a12e`.
- Active producer binary:
  `/mnt/bn/baiweikang/qianchuan_distill/awi/runtime/production-20261022/bin/awi-priorityfix-20261022`.
- Installed reader binary:
  `/mnt/bn/baiweikang/qianchuan_distill/awi/runtime/production-20261022/bin/awi-ad9b79c`.
- Installed local binary: `/home/tiger/.local/bin/awi`, SHA256
  `4544209cf3ece90413b401674a28ecb245ae0d0d572cf6f3418fe9946b6bc314`.
- Rollback binary: `/home/tiger/.local/bin/awi.rollback-20261022`.

Current code gates:

- `cargo fmt --check`: passed.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed.
- Rust tests: 97 unit + 6 CLI + 2 MCP + 14 MVP + 3 search-intent +
  1 wire-protocol = 123 passed.
- Python tests after the 2026-10-22 A/B harness hardening: 43 passed.
- Ruff check and format checks for the full Python tree: passed.
- The CI workflow now runs the same 43 Python tests instead of only the two
  original helper modules.

## Runtime topology

| Component | Worker / host | Local state |
|---|---|---|
| fast sync | Bona master | `/tmp/awi-master-home-20261022` |
| fast producer | CPU worker `4421679` | `/tmp/awi-fast-prod-20261022-v2/index` |
| NFS producer | CPU worker `4421779` | `/tmp/awi-nfs-prod-20261022-v2/index` |
| fleet monitor | Bona master | runtime health directory |
| reader | Bona master | `/tmp/awi-automatic-refresh-20261022-1000/index` |

All AWI server, producer, fast-sync and fleet-monitor processes were stopped
cleanly on 2026-10-22 at the user's request. Published snapshots, local producer
state and recovery checkpoints were retained. At shutdown both full scans were
still incomplete: the fast root had discovered 3,289 directories with
`last_completed_ms=0`; the broad NFS root had discovered 821 directories with
`last_completed_ms=0`. Indexing must resume before final freshness evidence can
be produced.

Shared runtime:
`/mnt/bn/baiweikang/qianchuan_distill/awi/runtime/production-20261022`.

Clean publication:
`/mnt/bn/baiweikang/qianchuan_distill/awi/publications/automatic-refresh-20261022`.

The previous publication remains untouched at
`/mnt/bn/baiweikang/qianchuan_distill/awi/publications/automatic-refresh-20260930`
as rollback evidence. Before this deployment it was pruned to the active and one
previous generation per shard: fast `747971` + `747970`, NFS `56707` + `56595`.
This reduced 77 GB of fast generations plus the NFS staging copy to 4.4 GB fast
and 5.2 GB NFS.

## Verified recovery behavior

- NFS producer child SIGKILL: supervisor retained PID `535773`, restarted child
  PID `539562`, recorded `restarts=1`, retained fence generation 3, and the
  warm semantic sidecar started in 4.36 s.
- NFS supervisor + child SIGKILL: fleet monitor observed
  `worker_or_supervisor_unreachable` after its 180 s deadline and logged
  `FAILOVER_ALERT`. Manual restart created supervisor `539913`, claimed fence
  generation 4, preserved the local index, and resumed scanning.
- Cross-process stale-writer test:
  `stale_fenced_writer_cannot_flip_the_current_pointer` passes. The Rust
  publisher refuses to rename `current.json` when the shared fence generation
  no longer matches.
- NFS slow path was exercised naturally through repeated `D` states and partial
  generations; the reader always retained the last valid complete shard set.

## Production CRUD and rollback

Using a separate read-only collection over the live NFS shard:

1. Created `production-create-token-alt-20261022`; reader search found the file.
2. Changed it to `production-change-token-20261022`; old token disappeared and
   the new token was returned from generation 85.
3. Renamed `crud.txt` to `crud_renamed.txt`; the old path disappeared and the
   renamed path was returned from generation 101.
4. Deleted the renamed file; subsequent search returned no result.

Reader rollback test used isolated generation pointers over the retained shard:
activation advanced `138 -> 157`, then atomically returned `157 -> 138`.

## Semantic state

The shared semantic environments are materialized on both CPU workers and the
Bona master. Both collection shards report `semantic`, `available=true`, and
`generation_lag=0` for the generations they currently serve.

The MCP delivery probe passes in both `text_and_structured` and `structured`
modes with matching adapter/reader build IDs, visible previews, search IDs, and
inspect excerpts.

## Development-only benchmark

A 16-case `split=dev`, `candidate_pending_dual_review` benchmark was run while
the clean corpus was still building. It is a pipeline diagnostic only:

- code Recall@10: 1.0;
- overall Recall@10: 0.25;
- P50/P95: 106.4 / 164.2 ms;
- stale rate: 0.

Dataset, SQL and experiment-result recall are not meaningful until their NFS
roots finish indexing. These values cannot open the release gate.

## Gold review and Agent A/B pilot

The original 16-case candidate was rejected by its first reviewers because
`result-001` referenced an incomplete audit and all baseline counts were
unverified. The set was corrected, the 16 original shell traces were preserved,
and counts were normalized to one call per listed command (59 total). Two fresh
independent reviewers then approved all 16 cases:

- `reviewer-f-20261022`: 16 approve
- `reviewer-g-20261022`: 16 approve

Review JSON and command logs are stored under `validation/reviews/`. The gold
set is now `review_status=approved`, remains `split=dev`, and has not created a
frozen split.

A one-task/two-arm A/B pilot ran on idle CPU worker `4421797` against the
snapshot reader. Both arms answered correctly; baseline used 3 calls and 23.67
seconds, AWI used 1 call and 14.14 seconds. This validates the isolated
router/MCP/scoring path but is not the release A/B result.

The clean final candidate A/B used three repetitions of 12 structured tasks
(72 runs), an isolated `CODEX_HOME`/`TMPDIR`, a hidden host
`.codex`/`.agents` mount, and a pinned collection (`fast 1169`, `NFS 1124`).
Results:

- baseline: 36/36 correct, 144 Agent tool calls, 647.78 s;
- AWI: 36/36 correct, 57 Agent tool calls, 485.18 s;
- paired reductions: 60.42% calls, 66.67% searches, 25.10% wall time;
- wall-delta mean 95% bootstrap CI: `[2.08, 6.87]` seconds;
- scope violations 0, policy violations 0, timeouts 0.

The tool-call gate passes, but the wall-time gate fails. A 30% reduction
requires AWI to finish within 453.44 s, so the run is 31.74 s over the ceiling.
Reviewer L and reviewer M independently rejected the result and identified
four task answer hints, one SQL label/value binding defect, a failed DuckDB
query followed by a retry, and an `experiment_result` regression. Their JSON
and logs are preserved under `validation/reviews/`, together with reviewer K's
earlier 48-run rejection.

The candidate tasks were then blinded and the runner hardened:

- removed the four reviewed answer hints;
- bound `generated_mid_key` to its qualified value while allowing the known
  qualified/unqualified variants;
- added `--allow-draft` for candidate pilots and hard rejection for frozen
  splits;
- added MCP argument/result-path capture and AWI-specific scope checks;
- added JSON-object inspection and no-retry guidance;
- added ten unit tests for guards, label binding, blinding and AWI scope.

Current task SHA256 is
`0e6dd21209d2115ba2b61aad2c6ab68520c9b6281142bb4b695f323a6ad1fa22`.
These revisions have not yet been used for a pilot or a replacement A/B run, so
the previous 72-run result is iteration evidence only.

## Producer priority ordering fix

Explicit finalized artifacts were observed waiting behind a large scan batch.
The ledger selected paths by `priority DESC`, but `notify_paths_budgeted` then
sorted the same batch lexicographically; with a bounded batch budget, background
paths could starve explicit events. The function now preserves caller ordering
while deduplicating, with regression test
`workspace::tests::notify_preserves_priority_input_order`.

Producer binary `awi-priorityfix-20261022` was built from the fix with SHA256
`64188549178e4d8bf2e48202f6d714ce513855296bfe2d290ab4535a34034630` and installed
in the runtime bin directory. Fast and NFS producers were restarted on this
binary under their existing supervisors and fence generations. The first
version, `awi-priorityfix-20261022-v1`, is retained for rollback.

After priority isolation and the finalized-artifact whitelist took effect, the
approved development benchmark on the namespace reader reached:

- Recall@10: `1.0`
- MRR: `0.769345`
- nDCG@10: `0.830109`
- P50/P95: `128.19 / 196.02 ms`
- stale-result rate: `0`
- tool-call reduction: `72.88%`

All four gate booleans passed. This run is retained as a pre-completion
diagnostic because the full per-root scans still have backlog; the formal
benchmark will be rerun and recorded after they complete.

## Remaining release gates

- Complete both full scans and report per-root completion, active files, symbols,
  datasets and failures.
- Re-run the reviewed benchmark against the completed collection and require
  Recall@10 >= 0.95, P95 <= 200 ms and stale rate 0.
- Run targeted `sql-pair-chain` and `result-v47-audit` pilots, then a fresh
  72-run A/B. Require at least 50% fewer tool calls and at least 30% lower
  aggregate wall time without weakening the gate. Investigate the
  `experiment_result` regression separately.
- Obtain two fresh independent approvals for the current task hash and raw
  traces before setting the A/B tasks to `review_status=approved`.
- Tag `v0.2.2` only after the gates pass; verify Linux x86_64/arm64 and macOS
  arm64/x86_64 release artifacts, checksums, installers and client integration.
