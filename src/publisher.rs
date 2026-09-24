use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::{Duration, Instant};
use std::{env, fs};

use anyhow::{Context, Result};
use ignore::WalkBuilder;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher, event::ModifyKind};
use serde::Serialize;

use crate::WorkspaceIndex;
use crate::extract::is_default_excluded;
use crate::model::IndexOptions;
use crate::snapshot::prune_generations;

/// Remote filesystem types whose events inotify cannot observe reliably. Roots
/// on these mounts fall back to periodic reconcile instead of real-time watch.
const REMOTE_FS_TYPES: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb",
    "smb3",
    "smbfs",
    "fuse.sshfs",
    "9p",
    "ceph",
    "glusterfs",
];

/// Configuration for the persistent producer that keeps a shared publication
/// current by reconciling every indexed root and publishing a new immutable
/// snapshot whenever content actually changes. Local-disk roots are watched in
/// real time; remote (NFS-style) roots and a periodic safety net drive the rest.
#[derive(Debug, Clone)]
pub struct PublisherConfig {
    /// Shared publication directory that snapshot-mode readers follow.
    pub publish_dir: PathBuf,
    /// Maximum delay between reconcile cycles; also the periodic safety net that
    /// covers remote roots where filesystem events are unavailable.
    pub interval: Duration,
    /// Quiet period after a filesystem event before reconciling, so bursts of
    /// edits coalesce into a single publish.
    pub debounce: Duration,
    /// Number of newest published generations to retain, including the active
    /// one. Older generations are pruned after each successful publish.
    pub retain: usize,
    /// Indexing limits applied to every reconcile.
    pub options: IndexOptions,
}

/// Outcome of a single reconcile-and-publish cycle.
#[derive(Debug, Clone, Serialize)]
pub struct PublishCycle {
    /// Sum of indexed (new or changed) files across every root this cycle.
    pub changed_files: u64,
    /// Sum of deleted files across every root this cycle.
    pub deleted_files: u64,
    /// Whether a new snapshot was published this cycle.
    pub published: bool,
    /// The generation number published this cycle, if any.
    pub generation: Option<i64>,
    /// Generations removed by retention pruning this cycle.
    pub pruned: Vec<i64>,
}

/// Resolve non-memory roots to reconcile. Explicit roots win; otherwise use
/// catalog roots that are not refreshed through the memory project registry.
pub fn resolve_roots(workspace: &WorkspaceIndex, roots: &[PathBuf]) -> Result<Vec<PathBuf>> {
    if roots.is_empty() {
        return workspace.indexed_roots();
    }
    roots
        .iter()
        .map(|root| {
            fs::canonicalize(root).with_context(|| format!("resolve watch root {}", root.display()))
        })
        .collect()
}

/// Classify each root as local (real-time watchable) or remote (periodic only)
/// by matching its longest mount prefix in `/proc/mounts`.
pub fn partition_roots(roots: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mounts = read_mounts();
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for root in roots {
        if is_remote_path(root, &mounts) {
            remote.push(root.clone());
        } else {
            local.push(root.clone());
        }
    }
    (local, remote)
}

/// Reconcile every root once and publish a new snapshot only when the content
/// changed or no snapshot has been published yet. Retention pruning runs after
/// each successful publish so the shared directory cannot grow without bound.
pub fn publish_once(
    workspace: &mut WorkspaceIndex,
    roots: &[PathBuf],
    config: &PublisherConfig,
) -> Result<PublishCycle> {
    let memory = workspace.refresh_agent_memories(&config.options)?;
    let mut changed_files = memory.indexed;
    let mut deleted_files = memory.deleted;
    for root in roots {
        let report = workspace.index_root(root, &config.options)?;
        changed_files += report.indexed;
        deleted_files += report.deleted;
    }

    let never_published = !config.publish_dir.join("current.json").exists();
    let mut cycle = PublishCycle {
        changed_files,
        deleted_files,
        published: false,
        generation: None,
        pruned: Vec::new(),
    };
    if changed_files + deleted_files > 0 || never_published {
        let manifest = workspace.publish_snapshot(&config.publish_dir)?;
        cycle.published = true;
        cycle.generation = Some(manifest.generation);
        cycle.pruned = prune_generations(&config.publish_dir, config.retain)?;
    } else {
        workspace.seal_semantic_generation()?;
    }
    // Reconciling every cycle appends generation rows even when nothing
    // changed; trim the bookkeeping so background filesystem noise cannot grow
    // the catalog without bound.
    let _ = workspace.prune_generation_history();
    Ok(cycle)
}

/// Run the producer forever. Local-disk roots are watched in real time so edits
/// publish within the debounce window; a periodic tick every `interval` is the
/// safety net that also covers remote roots where inotify is unavailable. A
/// failed cycle is logged and retried instead of aborting, and if the watcher
/// cannot start the producer degrades cleanly to pure periodic reconcile.
pub fn watch(index_dir: &Path, roots: &[PathBuf], config: &PublisherConfig) -> Result<()> {
    let mut workspace = WorkspaceIndex::open(index_dir)
        .with_context(|| format!("open AWI index {}", index_dir.display()))?;
    let resolved = resolve_roots(&workspace, roots)?;
    if resolved.is_empty() {
        anyhow::bail!(
            "watch requires at least one root; pass --root <path> or reconcile a root first"
        );
    }
    let (local, remote) = partition_roots(&resolved);

    // Keep the watcher alive for the whole loop; dropping it stops events.
    let (mut events, mut _watcher) = match start_watcher(&local) {
        Ok(handle) => handle,
        Err(error) => {
            eprintln!("AWI producer real-time watch unavailable; using periodic only: {error:#}");
            (channel().1, None)
        }
    };
    let watching_live = _watcher.is_some() && !local.is_empty();
    eprintln!(
        "AWI producer watching {} root(s) (real-time: {}, periodic-only: {}), \
         publishing to {} (interval {:?}, debounce {:?}, retain {})",
        resolved.len(),
        if watching_live { local.len() } else { 0 },
        if watching_live {
            remote.len()
        } else {
            resolved.len()
        },
        config.publish_dir.display(),
        config.interval,
        config.debounce,
        config.retain
    );

    // Reconcile once at startup so a fresh publication exists immediately.
    run_cycle(&mut workspace, &resolved, config);
    let mut last_cycle = Instant::now();

    loop {
        // Block until either a filesystem event arrives or the periodic tick
        // fires. An event reconciles only the local roots it can pertain to;
        // the timeout reconciles every root and is the sole trigger for remote
        // (NFS-style) roots, which have no reliable events.
        let cycle_roots: &[PathBuf] = match events.recv_timeout(config.interval) {
            Ok(_) => {
                drain_debounced(&events, config.debounce);
                // Enforce a minimum spacing between event-driven reconciles so
                // continuous background filesystem noise (editors, VCS, build
                // tools) cannot spin the loop. The debounce window is that floor.
                let since = last_cycle.elapsed();
                if since < config.debounce {
                    thread::sleep(config.debounce - since);
                    drain_debounced(&events, Duration::ZERO);
                }
                // A directory create event is observed by its watched parent,
                // but later writes beneath that new directory need their own
                // non-recursive watch.
                if let Ok((refreshed_events, refreshed_watcher)) = start_watcher(&local) {
                    events = refreshed_events;
                    _watcher = refreshed_watcher;
                }
                &local
            }
            Err(RecvTimeoutError::Timeout) => &resolved,
            Err(RecvTimeoutError::Disconnected) => {
                // Watcher thread gone: fall back to a plain periodic sleep and
                // keep covering every root on the timeout cadence.
                thread::sleep(config.interval);
                &resolved
            }
        };
        if !cycle_roots.is_empty() {
            run_cycle(&mut workspace, cycle_roots, config);
            last_cycle = Instant::now();
        }
    }
}

/// Reconcile and publish once, logging the outcome. Errors are surfaced but do
/// not abort the producer.
fn run_cycle(workspace: &mut WorkspaceIndex, roots: &[PathBuf], config: &PublisherConfig) {
    match publish_once(workspace, roots, config) {
        Ok(cycle) if cycle.published => eprintln!(
            "AWI producer published generation {} (changed={}, deleted={}, pruned={:?})",
            cycle.generation.unwrap_or_default(),
            cycle.changed_files,
            cycle.deleted_files,
            cycle.pruned
        ),
        Ok(_) => {}
        Err(error) => eprintln!("AWI producer cycle failed; retrying next tick: {error:#}"),
    }
}

/// Start non-recursive watches on every indexed directory. Enumerating with the
/// same ignore rules as indexing avoids notify's recursive traversal through
/// excluded trees and symlinked mounts.
fn start_watcher(local_roots: &[PathBuf]) -> Result<(Receiver<()>, Option<RecommendedWatcher>)> {
    if local_roots.is_empty() {
        return Ok((channel().1, None));
    }
    let (sender, receiver) = channel();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        // Wake only on content-changing events. Reconcile reads every file each
        // cycle, which itself emits access/open/close events under the watched
        // roots; reacting to those would self-trigger an endless reconcile loop.
        // Also drop events confined to excluded paths (VCS/cache/build churn).
        if let Ok(event) = result {
            if env::var_os("AWI_WATCH_DEBUG").is_some() {
                eprintln!(
                    "AWI watcher event kind={:?} paths={:?}",
                    event.kind, event.paths
                );
            }
            if !is_content_change(&event.kind) {
                return;
            }
            let relevant = event.paths.is_empty()
                || event
                    .paths
                    .iter()
                    .any(|path| !is_default_excluded(path) && !is_deferred_realtime_path(path));
            if relevant {
                let _ = sender.send(());
            }
        }
    })
    .context("create filesystem watcher")?;
    for target in watcher_targets(local_roots) {
        watcher
            .watch(&target, RecursiveMode::NonRecursive)
            .with_context(|| format!("watch root {}", target.display()))?;
    }
    Ok((receiver, Some(watcher)))
}

fn watcher_targets(local_roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut targets = BTreeMap::<PathBuf, ()>::new();
    for root in local_roots {
        if root.is_dir() {
            let mut builder = WalkBuilder::new(root);
            builder
                .hidden(false)
                .git_ignore(true)
                .git_exclude(true)
                .parents(true)
                .follow_links(false)
                .add_custom_ignore_filename(".awiignore");
            for entry in builder
                .filter_entry(|entry| !is_default_excluded(entry.path()))
                .build()
                .filter_map(|entry| entry.ok())
            {
                if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                    targets.insert(entry.into_path(), ());
                }
            }
        } else if root.exists() {
            targets.insert(root.parent().unwrap_or(root).to_owned(), ());
        }
    }
    targets.into_keys().collect()
}

fn is_content_change(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_)
            | EventKind::Remove(_)
            | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Name(_))
    )
}

fn is_deferred_realtime_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("log" | "jsonl" | "ndjson" | "csv" | "tsv" | "parquet")
    )
}

/// After the first event, keep absorbing events until the filesystem stays
/// quiet for one debounce window, so an edit burst yields a single reconcile.
fn drain_debounced(events: &Receiver<()>, debounce: Duration) {
    if debounce.is_zero() {
        while events.try_recv().is_ok() {}
        return;
    }
    let deadline = Instant::now() + debounce;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match events.recv_timeout(remaining) {
            Ok(_) => continue,
            Err(_) => return,
        }
    }
}

/// One (mount point, filesystem type) pair from `/proc/mounts`.
struct MountEntry {
    target: PathBuf,
    fs_type: String,
}

fn read_mounts() -> Vec<MountEntry> {
    let Ok(content) = fs::read_to_string("/proc/mounts") else {
        return Vec::new();
    };
    parse_mounts(&content)
}

fn parse_mounts(content: &str) -> Vec<MountEntry> {
    content
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let _source = fields.next()?;
            let target = fields.next()?;
            let fs_type = fields.next()?;
            Some(MountEntry {
                // /proc/mounts escapes spaces as \040; decode for accurate prefixes.
                target: PathBuf::from(unescape_mount(target)),
                fs_type: fs_type.to_owned(),
            })
        })
        .collect()
}

/// Decide whether a path lives on a remote filesystem by matching its longest
/// mount prefix. Unknown mounts are treated as local (real-time), matching the
/// common case that only network mounts need the periodic fallback.
fn is_remote_path(path: &Path, mounts: &[MountEntry]) -> bool {
    let mut best: Option<(usize, bool)> = None;
    for mount in mounts {
        if path.starts_with(&mount.target) {
            let depth = mount.target.components().count();
            let remote = REMOTE_FS_TYPES.iter().any(|kind| {
                mount.fs_type == *kind || mount.fs_type.starts_with(&format!("{kind}."))
            });
            if best.is_none_or(|(best_depth, _)| depth > best_depth) {
                best = Some((depth, remote));
            }
        }
    }
    best.map(|(_, remote)| remote).unwrap_or(false)
}

fn unescape_mount(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_owned();
    }
    let mut output = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            let octal: String = (&mut chars).take(3).collect();
            if octal.len() == 3
                && let Ok(code) = u8::from_str_radix(&octal, 8)
            {
                output.push(code as char);
                continue;
            }
            output.push('\\');
            output.push_str(&octal);
        } else {
            output.push(ch);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fs;

    use notify::event::{AccessKind, DataChange, MetadataKind};
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn watcher_accepts_content_changes_but_ignores_reads_and_metadata() {
        assert!(is_content_change(&EventKind::Create(
            notify::event::CreateKind::File
        )));
        assert!(is_content_change(&EventKind::Modify(ModifyKind::Data(
            DataChange::Content
        ))));
        assert!(is_content_change(&EventKind::Modify(ModifyKind::Name(
            notify::event::RenameMode::Any
        ))));
        assert!(is_content_change(&EventKind::Remove(
            notify::event::RemoveKind::File
        )));
        assert!(!is_content_change(&EventKind::Access(AccessKind::Any)));
        assert!(!is_content_change(&EventKind::Modify(ModifyKind::Any)));
        assert!(!is_content_change(&EventKind::Modify(ModifyKind::Other)));
        assert!(!is_content_change(&EventKind::Modify(
            ModifyKind::Metadata(MetadataKind::AccessTime)
        )));
    }

    #[test]
    fn realtime_watch_defers_append_heavy_data_files() {
        for path in [
            "run.log",
            "events.jsonl",
            "events.ndjson",
            "rows.csv",
            "rows.tsv",
            "part.parquet",
        ] {
            assert!(is_deferred_realtime_path(Path::new(path)));
        }
        assert!(!is_deferred_realtime_path(Path::new("src/main.rs")));
        assert!(!is_deferred_realtime_path(Path::new("docs/plan.md")));
    }

    #[test]
    fn watcher_skips_missing_registered_roots() {
        let missing = PathBuf::from("/tmp/awi-missing-root-parent/SKILL.md");
        let result = start_watcher(&[missing]);
        assert!(result.is_ok());
    }

    #[test]
    fn watcher_targets_skip_generated_directories_and_symlinks() {
        let fixture = tempdir().unwrap();
        let root = fixture.path().join("workspace");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join(".codex-work/task/src")).unwrap();
        fs::create_dir_all(root.join(".worktrees/feature/src")).unwrap();
        std::os::unix::fs::symlink("/tmp", root.join("external")).unwrap();

        let targets = watcher_targets(std::slice::from_ref(&root));
        assert!(targets.contains(&root));
        assert!(targets.contains(&root.join("src")));
        assert!(
            !targets
                .iter()
                .any(|path| path.starts_with(root.join(".codex-work")))
        );
        assert!(
            !targets
                .iter()
                .any(|path| path.starts_with(root.join(".worktrees")))
        );
        assert!(!targets.contains(&root.join("external")));
    }

    #[test]
    fn classifies_remote_and_local_mounts_by_longest_prefix() {
        let mounts = parse_mounts(
            "rootfs / ext4 rw 0 0\n\
             addr:/ /mnt/bn/baiweikang nfs4 rw 0 0\n\
             /dev/sda /home ext4 rw 0 0\n",
        );
        // NFS mount => remote.
        assert!(is_remote_path(
            Path::new("/mnt/bn/baiweikang/qianchuan_distill"),
            &mounts
        ));
        // Local ext4 home => not remote.
        assert!(!is_remote_path(
            Path::new("/home/tiger/Projects/Bona"),
            &mounts
        ));
        // Longest prefix wins: /home is ext4 even though / is also ext4.
        assert!(!is_remote_path(Path::new("/home"), &mounts));
        // Unknown path defaults to local.
        assert!(!is_remote_path(Path::new("/nonexistent/path"), &mounts));
    }

    #[test]
    fn decodes_escaped_mount_targets() {
        let mounts = parse_mounts("src /mnt/space\\040dir nfs4 rw 0 0\n");
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].target, PathBuf::from("/mnt/space dir"));
        assert!(is_remote_path(Path::new("/mnt/space dir/sub"), &mounts));
    }

    #[test]
    fn partition_splits_local_from_remote() {
        // Only paths that exist can be canonicalized, so exercise the mount
        // classifier directly with synthetic mounts.
        let mounts = parse_mounts("rootfs / ext4 rw 0 0\naddr:/ /mnt/bn nfs4 rw 0 0\n");
        let roots = [PathBuf::from("/mnt/bn/x"), PathBuf::from("/home/tiger/x")];
        let remote: Vec<_> = roots
            .iter()
            .filter(|root| is_remote_path(root, &mounts))
            .collect();
        assert_eq!(remote, vec![&PathBuf::from("/mnt/bn/x")]);
    }
}
