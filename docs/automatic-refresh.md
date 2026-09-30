# Resumable automatic refresh

`awi produce` is the supervised alternative to legacy `awi watch`. A producer
owns one local mutable index and one shared immutable publication. Use separate
producers for a small workspace/memory shard and the broad NFS shard. All
CPU-intensive indexing, profiling and embedding belongs on CPU workers.

## Producer

```bash
awi --index-dir /tmp/awi-fast/index produce \
  --root /workspace/project \
  --project-root /workspace/project \
  --skill-dir /home/user/.agents/skills \
  --publish-dir /shared/awi/fast \
  --inbox /shared/awi/events/fast \
  --status-file /shared/awi/health/fast.json \
  --scan-interval-ms 60000 --memory-interval-ms 60000 \
  --publish-interval-ms 30000 --batch-files 512 --batch-budget-ms 2000
```

- Register only concrete workspace roots, ancestor `AGENTS.md` files and Skill
  directories. `--project-root` discovers curated cross-Agent memory and Git's
  linked-worktree registry. It never indexes whole Agent home directories.
- `--skill-dir` discovers only `SKILL.md` files, to depth four. Linked worktrees
  and newly discovered manifests become independent registered scan roots.
- Existing `.gitignore`, `.awiignore`, sensitive-file and generated-directory
  exclusions still apply. Keep publication, runtime, model and checkpoint trees
  excluded from broad NFS roots to prevent self-indexing.
- An interval is a due-time schedule, not a scan-completion guarantee. Directory
  discovery, file indexing and deletion checks run in bounded batches. Indexing
  checks its deadline between files; individual I/O, parser, embedding, commit
  and publication calls may exceed the soft deadline.
- NFS writes require explicit events or periodic scanning; inotify is only a
  local hint. Continuous events cannot postpone the independent schedules.

File producers can enqueue an event while AWI is busy or stopped:

```bash
awi enqueue --inbox /shared/awi/events/fast /workspace/project/src/changed.rs
```

Atomic JSON event files remain on shared storage until their work or pending
queue is checkpointed against a published snapshot. Explicit events have
priority over discovery, paths deduplicate by revision, and acknowledging an old
revision cannot remove a newer event. Retry delay increases up to 256 seconds.
Discovery applies backpressure above eight ready batches.

## Persistence and deletion

`producer.sqlite3` is a local WAL database containing the queue, receipts,
publication dirty flag and directory frontier. It is never opened on NFS.
Frontier advancement and enqueued files commit together. A process restart
replays the currently interrupted directory; completed directories survive.
No traversal-position offset is reused against a changing directory listing.

Deletion uses catalog pages and individual filesystem checks. Only `ENOENT`
permits a tombstone. Permission, I/O and traversal errors retain entries and
retry. A scan cannot claim completion while a directory remains inaccessible;
`last_completed_ms` describes scanning, while pending queue counts separately
describe indexing backlog.

Every published snapshot includes `producer-state.sqlite3`. A newer standalone
`producer-checkpoint.sqlite3` records no-change traversal progress and pending
events without republishing the full index. It is atomically replaced and tied
to the exact snapshot pointer, including its manifest hash.

If the worker's local index is absent, `produce` verifies and materializes the
latest snapshot automatically, then restores a matching checkpoint. It never
bootstraps from an unfinished catalog. A nonempty target without a catalog is
rejected. Local and publication leases prevent concurrent writers. Full worker
replacement still requires assigning an available CPU worker and restarting the
same configuration there; the external monitor reports worker loss.

For initial migration, `--bootstrap-source <existing-publication>` supplies a
verified seed only when both the local catalog and the new producer's own
publication are absent. Subsequent recovery always prefers its own publication.
The seed remains untouched. Preserve any overlapping registered roots explicitly
in the producer configuration so their catalog-owned files receive deletion scans.

Before publication, the exclusive local writer finishes Tantivy merges and
collects obsolete segment files. LanceDB compacts current data and prunes committed
history; unverified fragments retain their recovery grace period. Immutable
published generations are unaffected.

## Reader collection

Place `collection.json` in a new source directory:

```json
{
  "format_version": 1,
  "shards": [
    {"name": "fast", "source": "../fast"},
    {
      "name": "nfs",
      "source": "../nfs",
      "exclude_paths": ["/workspace/project", "/home/user/.agents"],
      "exclude_kinds": ["agent_memory"]
    }
  ]
}
```

Then run `awi serve --snapshot-source /shared/awi/collection`. Readers pin all
shard pointers, verify changed copies, and atomically activate one complete set.
They reuse unchanged shard caches. A failed refresh keeps the previous set.
Ownership exclusions apply before retrieval Top-K and prevent deleted fast
files from reappearing through old base copies. Include every fast-owned path
in the base exclusions, or exclude a containing home/workspace scope.

Status reports each shard's generation separately. Generations from different
catalogs are not comparable timestamps. Aggregate physical file counts include
excluded base copies. A single-snapshot source remains compatible.

Collections merge per-channel evidence before recomputing RRF over the visible
candidate union. BM25 scores still use shard-local corpus statistics. Repeated
JSON/table results from the same directory receive progressive diversification;
source and SQL files retain the original small directory penalty. This is a new
ranking regime and needs a development benchmark; it cannot promise identical
rankings to a single catalog.

Local caches retain three recent activations plus every leased reader. Collection
descriptors protect their referenced shard copies. A shared reader lease prevents
garbage collection of a generation while a request is using it.

## Supervision

Run `python3 scripts/producer_supervisor.py config.json` under an independently
managed worker process. Example configuration:

```json
{
  "command": ["awi", "--index-dir", "/tmp/awi-fast/index", "produce",
    "--root", "/workspace/project", "--publish-dir", "/shared/awi/fast",
    "--inbox", "/shared/awi/events/fast",
    "--status-file", "/shared/awi/health/fast.json"],
  "heartbeat": "/shared/awi/health/fast.json",
  "status": "/shared/awi/health/fast-supervisor.json",
  "log": "/shared/awi/logs/fast.log",
  "startup_seconds": 1800,
  "heartbeat_seconds": 120,
  "progress_seconds": 900
}
```

It checks the exact child PID, startup, heartbeat and useful progress, restarts
only its owned process group, backs off up to 60 seconds and rotates logs at
32 MiB with three backups. Healthy idle periods need no new publication.
Configure the progress deadline above the longest expected seal/copy/embedding
operation. Reported batch budgets are soft; this is the independent hard limit.

On another host, run `--monitor-only` with a configuration containing `status`,
`timeout_seconds` and `producers: [{"name":"fast","status":"...supervisor.json"}]`.
Its fleet status distinguishes a stopped/restarting process from missing worker
or supervisor heartbeats. It does not send messages or allocate replacement
resources.

Watch heartbeat age, `phase`, `last_error`, `pending_paths`, `oldest_event_ms`,
`dirty`, per-root `last_completed_ms`, and reader shard generations. An old
`current.json` alone is not failure when no content changed.

## Validation and rollback

Run Rust fmt, release Clippy with all targets/features, full Rust tests and
`python3 scripts/test_producer_supervisor.py` on a CPU worker. The tests cover
revision replay, inaccessible directories, publication failure, recovery,
expired batch deadlines and watchdog restart/idle behavior.

Benchmark the actual collection with
`awi --index-dir /tmp/benchmark-cache benchmark --snapshot-source <collection>
--gold <dev.json> --allow-draft --output <report.json>`.
Never consume frozen evaluation inputs or call a draft score a reviewed release
gate. Verify create/change/rename/delete, new worktrees, and reader visibility
while the NFS producer is still scanning. Test one owned producer restart.

Before activation, preserve the previous binary, MCP wrapper and valid
single-snapshot publication. Rollback restores that wrapper/source, stops the
exact new reader and restarts the compatible previous reader. Do not overwrite
or prune the retained rollback publication during validation.

## Bona deployment

See [2026-09-30 deployment record](automatic-refresh-deployment-20260930.md) for active workers, configuration, evidence, benchmark limits and rollback.
