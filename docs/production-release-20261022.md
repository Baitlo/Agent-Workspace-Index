# AWI production restoration and release evidence — 2026-10-22

Status: in progress. This file records authoritative evidence as the clean
production generation is built; it must not be read as a completed release gate
until the checklist at the end is complete.

## Code baseline

- Code commit at the start of deployment: `21da0b4`.
- Release binary: `/mnt/bn/baiweikang/qianchuan_distill/awi/runtime/production-20261022/bin/awi-21da0b4`.
- Installed local binary: `/home/tiger/.local/bin/awi`, SHA256
  `5b43a5da40ceaff6ecb2e9f6d54a8628f8c4b1272d5c131253d1b7ddc1c69c3a`.
- Rollback binary: `/home/tiger/.local/bin/awi.rollback-20261022`.

Current code gates:

- `cargo fmt --check`: passed.
- `cargo clippy --locked --all-targets --all-features -- -D warnings`: passed.
- Rust tests: 95 unit + 6 CLI + 2 MCP + 14 MVP + 3 search-intent +
  1 wire-protocol = 121 passed.
- Python tests: 33 passed.
- Ruff checks for semantic, audit, supervisor and task-outcome helpers: passed.

## Runtime topology

| Component | Worker / host | Local state |
|---|---|---|
| fast sync | Bona master | `/tmp/awi-master-home-20261022` |
| fast producer | CPU worker `4421679` | `/tmp/awi-fast-prod-20261022-v2/index` |
| NFS producer | CPU worker `4421779` | `/tmp/awi-nfs-prod-20261022-v2/index` |
| fleet monitor | Bona master | runtime health directory |
| reader | Bona master | `/tmp/awi-automatic-refresh-20261022-1000/index` |

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

## Remaining release gates

- Complete both full scans and report per-root completion, active files, symbols,
  datasets and failures.
- Require two independent review results for the 16-case gold set, adjudicate
  disagreements, and only then mark the set approved.
- Re-run the reviewed benchmark against the completed collection and require
  Recall@10 >= 0.95, P95 <= 200 ms and stale rate 0.
- Run the real-Agent paired A/B and report tool calls, wall time, tokens, paired
  reductions and confidence intervals.
- Tag `v0.2.2` only after the gates pass; verify Linux x86_64/arm64 and macOS
  arm64/x86_64 release artifacts, checksums, installers and client integration.
