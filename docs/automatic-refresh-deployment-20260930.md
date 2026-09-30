# Bona automatic refresh deployment — 2026-09-30

Activated on the existing MCP socket. This is a local deployment, not a tagged public release. The broad NFS first scan is still running.

## Runtime

Shared runtime: `/mnt/bn/baiweikang/qianchuan_distill/awi/runtime/automatic-refresh-20260930`.
Shared publication: `/mnt/bn/baiweikang/qianchuan_distill/awi/publications/automatic-refresh-20260930`.

| Component | Host | Local index / configuration |
|---|---|---|
| Fast producer | MLX CPU 4368061, `[fdbd:dc05:13:43c::50]:11755` | `/tmp/awi-fast-20260930/index`; runtime `fast.json` |
| NFS producer | MLX CPU 4352003, `[fdbd:dc05:17:a::15]:9214` | `/tmp/awi-nfs-20260930/index`; runtime `nfs.json` |
| Reader | Bona master | `/tmp/awi-automatic-refresh-1000/index` |
| Fleet monitor | Bona master | runtime `monitor.json`, `--monitor-only` |

The installed wrapper is `~/.local/bin/awi-mcp`; the stable socket remains `~/.local/state/awi/codex-router/awi.sock`. Existing adapters use that socket. The wrapper restarts an absent reader on invocation. Producers run under `scripts/producer_supervisor.py`, launched with `setsid nohup`.

The fast producer scans the full Bona workspace, refreshes project memory, discovers linked worktrees through Bona and AWI project registrations, and discovers bounded Skill manifests. Linked Skills resolve only their manifest, avoiding recursive asset links. The NFS producer retains `/mnt/bn/baiweikang` and the two previously registered dataset roots. Existing `.awiignore` exclusions remain.

Both were seeded from verified `candidate-v3` generation 38945 into new local indexes. The unfinished historical NFS writer was not reused. Physical seed copies remain in each catalog; the fast reader excludes `/mnt`, and the NFS reader excludes `/home` and `agent_memory`.

Schedules: 60-second fast scans/memory discovery, 30-second fast publication, and 300-second NFS scan/publication. These are due times, not end-to-end guarantees: backlog, embedding, copies and NFS metadata visibility can extend freshness. Explicit `awi enqueue` events take priority.

## Validation

- Rust fmt, release Clippy all targets/features with warnings denied, 81 library tests and 13 integration tests passed; two Python supervisor tests passed.
- Real create/change/rename/delete reached the collection reader. New linked worktree and Skill were discovered. All fixtures were removed; deleted content no longer appeared.
- Fast generations advanced while NFS was still discovering files.
- Controlled SIGKILL of owned producer PID 42835 recovered automatically: supervisor restarted PID 54188 after approximately six seconds; pending work and traversal continued. PIDs subsequently changed during upgrades.
- The installed wrapper completed MCP initialize and workspace_search; `ProducerConfig bootstrap_source` returned updated `src/producer.rs` first.
- Deployed status: 524 visible memory files, stale=0, missing=0, source lag=0; both shards reported available semantics matching their own catalog generations. Hidden legacy NFS memory is excluded from aggregate memory.

Evidence under runtime: `mcp-validation.json`, `deployed-status.json`, `restart-before.json`, `supervisor-tests.log`, producer logs and `health/fleet.json`. Build/test logs on CPU 4368061: `/tmp/awi-refresh-{clippy,tests,build}.log`.

The 16-case draft development set was used; no frozen set or reviewed release gate was consumed. Reports: `/mnt/bn/baiweikang/qianchuan_distill/awi/evaluation/automatic-refresh-20260930`.

| Draft report | Recall@10 | MRR | nDCG@10 | P50 ms | P95 ms | Stale |
|---|---:|---:|---:|---:|---:|---:|
| single-shard.json (old corpus) | 1.000 | 0.7057 | 0.7774 | 218.4 | 258.5 | 0 |
| refreshed-single.json | 1.000 | 0.6536 | 0.7395 | 150.3 | 218.8 | 0 |
| collection-stable-window.json | 1.000 | 0.6635 | 0.7483 | 226.8 | 295.9 | 0 |

Producers actively updated the corpus, so these are development comparisons, not a fixed-corpus causal experiment. P95 still exceeds 200 ms. Global fusion uses a stable 200-candidate window per shard; BM25 corpus statistics remain shard-local. Further tuning needs broader gold coverage.

Compaction reduced initial roughly 12 GB seed snapshots to roughly 1.1 GB per shard before subsequent NFS growth. This is not a permanent size cap. Reader caches retain three activations plus leased readers.

## Operations and rollback

Read runtime `health/fleet.json`, then producer heartbeats for queue age, errors, progress and per-root completion. Full NFS completeness requires all relevant scans to complete. The master monitor detects worker loss; replacement allocation remains an operational action. It sends no external messages.

Restart a supervisor on its assigned CPU worker with `setsid nohup bash <runtime>/run-supervisor.sh <runtime>/fast.json >> <runtime>/logs/fast-launch.log 2>&1 < /dev/null &`; use `nfs.json` on the NFS worker. Verify the exact existing supervisor before stopping it. Status and publication leases reject duplicate owners.

Rollback assets: runtime `rollback/awi` and `rollback/awi-mcp`. Original `two_shot_plus_tongyong_qianchuan_retrieval_20260919/candidate-v3` remains untouched. Stop the exact current reader, atomically restore the saved binary/wrapper, then invoke the wrapper to restart the old single-snapshot reader. New producers may remain for diagnosis or be stopped by exact supervisor PID. Do not prune the rollback publication.
