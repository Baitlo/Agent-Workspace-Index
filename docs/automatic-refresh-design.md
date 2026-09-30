# Automatic refresh upgrade

Implementation plan, 2026-09-30. This document describes the intended contract;
features are not operational until the validation and deployment steps complete.

## Requirements

- Code, curated project memory, and Skill changes publish independently of broad
  NFS scans. Preserve the full configured NFS scope and existing exclusions.
- Persist concrete changed paths, deduplicate updates, retry failures, and replay
  after restart. Existing `notify_paths` supplies per-file indexing.
- Reconcile NFS incrementally with bounded batches and durable traversal progress.
  Never infer deletion from an incomplete or failed directory traversal.
- Supervise both process liveness and useful progress. A healthy idle producer
  must not be classified as stalled merely because no new snapshot was needed.
- Keep all mutable catalog, queue, scan state, vector and build files on local
  storage. Production bulk work runs on MLX CPU workers, not the master.
- Preserve the existing single-snapshot CLI/MCP behavior and rollback path.

## Reader and publication contract

Add an optional `collection.json` at a snapshot source. It lists named independent
snapshot sources and explicit path/kind exclusions for migration. Existing
`serve --snapshot-source` discovers this format; single snapshots keep working.
Each shard has one writer and the normal catalog/Tantivy/vector sealing checks.

The reader resolves a vector of shard pointers, materializes only changed shards,
and atomically activates a local collection descriptor. Every request pins one
complete descriptor. Refresh failure retains the previous complete descriptor.
Full pointer identities, including manifest hashes, detect changes; generation
integers from separate catalogs must never be compared as freshness timestamps.
Unchanged shard caches must not be recopied or rehashed on every fast update.

A reader abstraction wraps either one WorkspaceIndex or a set of shard readers.
Retain the daemon's bounded query pool and per-worker readers, sharing each
shard's semantic runtime across workers. Search fans out, applies scope ownership
and exclusions, merges deterministic scores, and deduplicates paths. Overfetch
must account for filtering; do not truncate before removing excluded results.
Inspect routes to the authoritative shard and must not fall back to an excluded
older copy after deletion. Query validates exact roots against the union and
executes the existing confined DuckDB query once, including cross-shard inputs.
Status exposes each shard's generation and freshness separately.

Initial migration can retain the last valid broad snapshot as the NFS base while
a fresh fast shard owns the workspace and curated Agent memory. Explicit
ownership/exclusions prevent the base from resurrecting old code or memory.
Activate only after quality and real-change checks pass.

## Producer contract

Keep legacy watch available. Add a resumable producer with:

- a local SQLite change ledger, stable event revisions, retry metadata and a
  dirty-publication flag;
- path notifications accepted independently of expensive indexing, and local
  filesystem events translated into path updates or directory scan requests;
- independent absolute schedules for memory rediscovery, directory discovery,
  NFS巡检 and publication, so continuous events cannot starve periodic tasks;
- bounded file/time batches, persistent directory frontier and scan status;
- guarded deletion checks in catalog pages, preserving entries on permission or
  transient filesystem errors;
- per-root completed-scan times, pending paths, oldest event age, scan cursor,
  indexing/publication progress, heartbeat and last error;
- atomic shared checkpoints/journals for worker-loss recovery, consistent with
  the last published catalog. Never place a SQLite WAL on NFS.

Avoid loading a full large file catalog or frontier into memory. A single
blocking filesystem call may exceed a soft batch budget; the external supervisor
provides a separate hard progress deadline and restarts from safe checkpoints.
Do not acknowledge source events in a way that loses un-published work after
process or worker failure.

The production supervisor restarts only its owned process group with backoff,
checks heartbeat and progress independently, rotates shared logs, and reports
worker loss externally. No blanket process kills.

## Verification

- Single-snapshot compatibility and deterministic federation.
- Fast shard advances while NFS remains unchanged; failed shard activation keeps
  the complete old collection; same generation with a different pointer is detected.
- Create, modify, rename, delete and ownership prevent stale fallback.
- Queue deduplication, a newer event during processing, crash/replay, failed
  publication, and safe checkpoint recovery.
- Multi-batch and interrupted traversal; inaccessible subtree; remote changes
  without events; newly discovered memory/worktree roots.
- Supervisor exit restart, progress timeout, healthy idle and backoff behavior.
- fmt, Clippy, full tests on a CPU worker; retrieval quality/latency benchmark
  using the existing draft development set; no frozen evaluation inputs.
- Real production update, reader visibility, rollback, and controlled process
  recovery before declaring deployment complete.
