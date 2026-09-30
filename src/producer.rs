//! Independently scheduled, resumable snapshot producer.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, sync_channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use fs4::fs_std::FileExt;
use ignore::{Walk, WalkBuilder};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher, event::ModifyKind};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::WorkspaceIndex;
use crate::change_ledger::{Ledger, now_ms};
use crate::extract::{is_default_excluded, is_sensitive};
use crate::model::IndexOptions;
use crate::publisher::partition_roots;
use crate::snapshot::{materialize_latest, prune_generations, read_pointer};
use crate::workspace::notification_path;

#[derive(Debug, Clone)]
pub struct ProducerConfig {
    pub publish_dir: PathBuf,
    pub bootstrap_source: Option<PathBuf>,
    pub inbox: PathBuf,
    pub status_file: PathBuf,
    pub roots: Vec<PathBuf>,
    pub project_roots: Vec<PathBuf>,
    pub skill_dirs: Vec<PathBuf>,
    pub scan_interval: Duration,
    pub memory_interval: Duration,
    pub publish_interval: Duration,
    pub batch_files: usize,
    pub batch_budget: Duration,
    pub retain: usize,
    pub options: IndexOptions,
}

#[derive(Serialize, Deserialize)]
struct Event {
    paths: Vec<PathBuf>,
}

/// Producers of files may call this while the indexer is busy or offline.
/// Files stay in the shared inbox until a matching publication/checkpoint exists.
pub fn enqueue(inbox: &Path, paths: &[PathBuf]) -> Result<PathBuf> {
    if paths.is_empty() || paths.len() > 10_000 {
        bail!("enqueue requires between 1 and 10000 paths");
    }
    fs::create_dir_all(inbox)?;
    let event = Event {
        paths: paths
            .iter()
            .map(|p| {
                if p.is_absolute() {
                    Ok(p.clone())
                } else {
                    Ok(std::env::current_dir()?.join(p))
                }
            })
            .collect::<Result<_>>()?,
    };
    let name = format!("{}-{}-{}.json", now_ms(), std::process::id(), unique_id());
    let target = inbox.join(name);
    atomic_json(&target, &event)?;
    Ok(target)
}

fn unique_id() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::create_dir_all(path.parent().context("JSON parent")?)?;
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = File::create(&temp)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temp, path)?;
    File::open(path.parent().context("JSON parent")?)?.sync_all()?;
    Ok(())
}

struct ActiveDirectory {
    id: i64,
    root: PathBuf,
    path: PathBuf,
    walker: Walk,
}

struct Runtime {
    started_ms: i64,
    last_progress_ms: i64,
    processed: u64,
    last_error: Option<String>,
}

impl Runtime {
    fn status(&self, ledger: &Ledger, config: &ProducerConfig, phase: &str) -> Result<()> {
        let (pending, oldest) = ledger.pending()?;
        atomic_json(
            &config.status_file,
            &serde_json::json!({
                "pid": std::process::id(), "started_ms": self.started_ms,
                "heartbeat_ms": now_ms(), "last_progress_ms": self.last_progress_ms,
                "phase": phase, "processed": self.processed, "pending_paths": pending,
                "oldest_event_ms": oldest, "dirty": ledger.dirty()?,
                "last_published_ms": ledger.get("last_published_ms")?,
                "last_error": self.last_error, "scans": ledger.scans()?,
            }),
        )
    }

    fn outcome<T>(&mut self, result: Result<T>) -> Option<T> {
        match result {
            Ok(value) => Some(value),
            Err(error) => {
                self.last_error = Some(format!("{error:#}"));
                eprintln!("AWI producer retryable error: {error:#}");
                None
            }
        }
    }
}

fn lock(path: &Path) -> Result<File> {
    fs::create_dir_all(path.parent().context("lock parent")?)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    if !file.try_lock_exclusive()? {
        bail!("producer already owns {}", path.display());
    }
    Ok(file)
}

/// Restore a writer only from a verified sealed snapshot, never an unfinished
/// catalog. The standalone checkpoint may be newer, but must match this pointer.
fn bootstrap(index: &Path, source: &Path) -> Result<()> {
    if index.join("catalog.sqlite3").exists() || !source.join("current.json").exists() {
        fs::create_dir_all(index)?;
        return Ok(());
    }
    let cache = index.with_extension("bootstrap");
    let activated = materialize_latest(source, &cache)?;
    if index.exists() {
        // Refuse to replace arbitrary contents or an interrupted local writer.
        fs::remove_dir(index).context("bootstrap target must be empty")?;
    }
    fs::rename(activated.path, index)?;
    let shared = source.join("producer-checkpoint.sqlite3");
    let state = index.join("producer.sqlite3");
    let checkpoint = index.join("producer-state.sqlite3");
    if checkpoint.exists() {
        fs::copy(&checkpoint, &state)?;
    }
    if shared.exists() {
        let candidate = index.join("recovery.sqlite3");
        let restore = (|| -> Result<()> {
            fs::copy(shared, &candidate)?;
            let ledger = Ledger::open(&candidate)?;
            if ledger.get("snapshot_pointer")?.as_deref()
                == Some(&serde_json::to_string(&read_pointer(source)?)?)
            {
                ledger.checkpoint(&state)?;
            }
            Ok(())
        })();
        if let Err(error) = restore {
            eprintln!("AWI standalone checkpoint unavailable; using sealed state: {error:#}");
        }
    }
    Ok(())
}

pub fn run(index: &Path, config: &ProducerConfig) -> Result<()> {
    if config.batch_files == 0
        || config.batch_budget.is_zero()
        || config.scan_interval.is_zero()
        || config.memory_interval.is_zero()
        || config.publish_interval.is_zero()
    {
        bail!("producer budgets and intervals must be positive");
    }
    let _local_lock = lock(&index.with_extension("producer.lock"))?;
    let _publication_lock = lock(&config.publish_dir.join("producer.lock"))?;
    let source = if config.publish_dir.join("current.json").exists() {
        &config.publish_dir
    } else {
        config
            .bootstrap_source
            .as_ref()
            .unwrap_or(&config.publish_dir)
    };
    bootstrap(index, source)?;
    let mut workspace = WorkspaceIndex::open(index)?;
    let ledger = Ledger::open(&index.join("producer.sqlite3"))?;
    let mut roots = if config.roots.is_empty() {
        workspace.indexed_roots()?
    } else {
        config
            .roots
            .iter()
            .map(|r| notification_path(r))
            .collect::<Result<Vec<_>>>()?
    };
    // Retain discovered roots after restart, including removed worktrees whose
    // old files still need a guarded deletion pass.
    roots.extend(ledger.roots()?);
    roots.sort();
    roots.dedup();
    if roots.is_empty() {
        bail!("producer requires at least one root");
    }
    for root in &roots {
        workspace.register_scan_root(root)?;
    }
    fs::create_dir_all(&config.inbox)?;
    let (events, mut watcher) = watcher()?;
    let mut active = None;
    let mut runtime = Runtime {
        started_ms: now_ms(),
        last_progress_ms: now_ms(),
        processed: 0,
        last_error: None,
    };
    let mut next_memory = Instant::now();
    let mut next_publish = Instant::now();
    loop {
        runtime.status(&ledger, config, "events")?;
        runtime.outcome(ingest(&ledger, &config.inbox));
        // Events are hints. Bound drainage so a continuous stream cannot starve
        // scan, memory or publication deadlines.
        for event in events.try_iter().take(config.batch_files).flatten() {
            for path in event.paths {
                runtime.outcome(ledger.enqueue(&path));
            }
        }
        if Instant::now() >= next_memory {
            next_memory = Instant::now() + config.memory_interval;
            runtime.status(&ledger, config, "memory_discovery")?;
            runtime.outcome(discover(&mut workspace, &ledger, &mut roots, config));
        }
        runtime.outcome(ledger.schedule(&roots, config.scan_interval.as_millis() as i64));
        runtime.status(&ledger, config, "scan")?;
        if let Some(count) = runtime.outcome(scan_batch(
            &workspace,
            &ledger,
            &mut active,
            &mut watcher,
            config,
        )) {
            runtime.processed += count;
            if count > 0 {
                runtime.last_progress_ms = now_ms();
            }
        } else {
            // The transaction rolled back: discard the advanced in-memory cursor.
            active = None;
        }
        runtime.status(&ledger, config, "indexing")?;
        runtime.outcome(apply_batch(&mut workspace, &ledger, &roots, config));
        if Instant::now() >= next_publish {
            next_publish = Instant::now() + config.publish_interval;
            runtime.status(&ledger, config, "publishing")?;
            if runtime
                .outcome(publish(&mut workspace, &ledger, config))
                .is_some()
            {
                runtime.last_progress_ms = now_ms();
            }
        }
        runtime.status(&ledger, config, "idle")?;
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn watcher() -> Result<(
    Receiver<notify::Result<notify::Event>>,
    Option<RecommendedWatcher>,
)> {
    let (sender, receiver) = sync_channel(8192);
    let result = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.as_ref().is_ok_and(|e| {
            matches!(
                e.kind,
                EventKind::Create(_)
                    | EventKind::Remove(_)
                    | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Name(_))
            )
        }) {
            let _ = sender.try_send(event);
        }
    });
    match result {
        Ok(watcher) => Ok((receiver, Some(watcher))),
        Err(error) => {
            eprintln!("AWI events unavailable; periodic scan remains active: {error}");
            Ok((receiver, None))
        }
    }
}

fn ingest(ledger: &Ledger, inbox: &Path) -> Result<()> {
    // Keep the files until publication. Receipt IDs deduplicate restarts.
    let mut ingested = 0;
    for entry in fs::read_dir(inbox)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let seen: bool = ledger.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM receipts WHERE name=?1)",
            [&name],
            |r| r.get(0),
        )?;
        if seen {
            continue;
        }
        if entry.metadata()?.len() > 4 * 1024 * 1024 {
            bail!("oversized producer event {}", path.display());
        }
        let event: Event = serde_json::from_slice(&fs::read(&path)?)?;
        let transaction = ledger.db.unchecked_transaction()?;
        for path in event.paths {
            ledger.enqueue(&path)?;
        }
        ledger
            .db
            .execute("INSERT INTO receipts VALUES (?1)", [&name])?;
        transaction.commit()?;
        ingested += 1;
        if ingested >= 64 {
            break;
        }
    }
    Ok(())
}

fn allowed(root: &Path, path: &Path) -> bool {
    if !path.starts_with(root) || is_sensitive(path) {
        return false;
    }
    if root == path {
        return true;
    }
    if path.strip_prefix(root).is_ok_and(is_default_excluded) {
        return false;
    }
    let mut builder = ignore::gitignore::GitignoreBuilder::new(root);
    // Preserve parent and nested custom ignores for explicit events as well as scans.
    let mut ancestors = path
        .parent()
        .into_iter()
        .flat_map(Path::ancestors)
        .collect::<Vec<_>>();
    ancestors.reverse();
    for ancestor in ancestors {
        builder.add(ancestor.join(".gitignore"));
        builder.add(ancestor.join(".awiignore"));
        builder.add(ancestor.join(".git/info/exclude"));
    }
    builder.build().is_ok_and(|ignore| {
        !ignore
            .matched_path_or_any_parents(path, path.is_dir())
            .is_ignore()
    })
}

fn apply_batch(
    workspace: &mut WorkspaceIndex,
    ledger: &Ledger,
    roots: &[PathBuf],
    config: &ProducerConfig,
) -> Result<()> {
    let deadline = Instant::now() + config.batch_budget;
    let changes = ledger.batch(config.batch_files)?;
    let mut files = Vec::new();
    for change in &changes {
        let result = (|| -> Result<bool> {
            let path = notification_path(&change.path)?;
            let root = roots
                .iter()
                .filter(|r| path.starts_with(r))
                .max_by_key(|r| r.components().count())
                .context("event is outside producer roots")?;
            if !allowed(root, &path) {
                return Ok(false);
            }
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() => {
                    ledger.directory(root, &path)?;
                    Ok(false)
                }
                Ok(meta) if meta.file_type().is_symlink() => Ok(false),
                Ok(_) => workspace.needs_update(&path),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // A removed directory has no file record. The paged deletion
                    // pass checks its descendants without inferring disappearance.
                    ledger.db.execute(
                        "UPDATE scans SET last_completed_ms=0 WHERE root=?1",
                        [root.to_string_lossy().as_ref()],
                    )?;
                    Ok(true)
                }
                Err(error) => Err(error.into()),
            }
        })();
        match result {
            Ok(true) => files.push(change),
            Ok(false) => ledger.ack(change)?,
            Err(error) => ledger.retry(change, &format!("{error:#}"))?,
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    if files.is_empty() {
        return Ok(());
    }
    // Mark dirty BEFORE writing either derived index; interrupted commits replay.
    ledger.set("dirty", "1")?;
    let paths = files.iter().map(|e| e.path.clone()).collect::<Vec<_>>();
    let result = workspace
        .notify_paths_budgeted(
            &paths,
            &config.options,
            Some(Instant::now() + config.batch_budget),
        )
        .and_then(|(report, attempted)| {
            if report.roots.iter().any(|r| r.failed > 0) {
                bail!("one or more notified files failed");
            }
            Ok(attempted)
        });
    match result {
        Ok(attempted) => {
            for change in files {
                if notification_path(&change.path).is_ok_and(|p| attempted.contains(&p)) {
                    ledger.ack(change)?;
                }
            }
        }
        Err(error) => {
            for change in files {
                ledger.retry(change, &format!("{error:#}"))?;
            }
            return Err(error);
        }
    }
    Ok(())
}

fn scan_batch(
    workspace: &WorkspaceIndex,
    ledger: &Ledger,
    active: &mut Option<ActiveDirectory>,
    watcher: &mut Option<RecommendedWatcher>,
    config: &ProducerConfig,
) -> Result<u64> {
    if ledger.ready_count()? >= (config.batch_files as i64).saturating_mul(8) {
        return Ok(0);
    }
    let transaction = ledger.db.unchecked_transaction()?;
    let deadline = Instant::now() + config.batch_budget;
    let mut count = 0;
    while count < config.batch_files as u64 && Instant::now() < deadline {
        if active.is_none() {
            let next = ledger.db.query_row(
                "SELECT id,root,path FROM directories WHERE retry_ms<=?1 ORDER BY retry_ms,id LIMIT 1",
                [now_ms()], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)),
            ).optional()?;
            let Some((id, root, path)) = next else {
                break;
            };
            let path = PathBuf::from(path);
            let root = PathBuf::from(root);
            if let Some(watcher) = watcher
                && !partition_roots(std::slice::from_ref(&path)).0.is_empty()
            {
                let target = if path.is_dir() {
                    &path
                } else {
                    path.parent().unwrap_or(&path)
                };
                let _ = watcher.watch(target, RecursiveMode::NonRecursive);
            }
            let mut builder = WalkBuilder::new(&path);
            builder
                .hidden(false)
                .parents(true)
                .git_ignore(true)
                .git_exclude(true)
                .follow_links(false)
                .max_depth(Some(1))
                .add_custom_ignore_filename(".awiignore");
            let scan_root = root.clone();
            builder.filter_entry(move |e| {
                e.path() == scan_root
                    || (!is_sensitive(e.path())
                        && !e
                            .path()
                            .strip_prefix(&scan_root)
                            .is_ok_and(is_default_excluded))
            });
            *active = Some(ActiveDirectory {
                id,
                root,
                path,
                walker: builder.build(),
            });
        }
        let directory = active.as_mut().expect("active directory");
        match directory.walker.next() {
            Some(Ok(entry)) => {
                count += 1;
                let path = entry.path();
                if entry.error().is_some() {
                    ledger.db.execute(
                        "UPDATE directories SET retry_ms=?2,error=?3 WHERE id=?1",
                        params![
                            directory.id,
                            now_ms() + 30_000,
                            entry.error().map(ToString::to_string)
                        ],
                    )?;
                    *active = None;
                    continue;
                }
                if entry.file_type().is_some_and(|t| t.is_dir()) {
                    if path != directory.path {
                        ledger.directory(&directory.root, path)?;
                    }
                } else if entry.file_type().is_some_and(|t| t.is_file()) {
                    // Queue even transient stat failures; later retries preserve them.
                    if workspace.needs_update(path).unwrap_or(true) {
                        ledger.enqueue_scan(path)?;
                    }
                }
            }
            Some(Err(error)) => {
                if fs::symlink_metadata(&directory.path)
                    .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
                {
                    // Authoritative ENOENT permits the separate per-file stat pass.
                    ledger
                        .db
                        .execute("DELETE FROM directories WHERE id=?1", [directory.id])?;
                } else {
                    ledger.db.execute(
                        "UPDATE directories SET retry_ms=?2,error=?3 WHERE id=?1",
                        params![directory.id, now_ms() + 30_000, error.to_string()],
                    )?;
                }
                *active = None;
                count += 1;
            }
            None => {
                ledger
                    .db
                    .execute("DELETE FROM directories WHERE id=?1", [directory.id])?;
                *active = None;
                count += 1;
            }
        }
    }
    ledger.db.execute(
        "UPDATE scans SET phase='deletion' WHERE phase='discovery'
         AND NOT EXISTS(SELECT 1 FROM directories d WHERE d.root=scans.root)",
        [],
    )?;
    let root = ledger.db.query_row(
        "SELECT root,after_id FROM scans WHERE phase='deletion' ORDER BY last_completed_ms,root LIMIT 1", [],
        |r| Ok((PathBuf::from(r.get::<_,String>(0)?), r.get::<_,i64>(1)?)),
    ).optional()?;
    if let Some((root, after)) = root {
        let page = workspace.scan_file_page(&root, after, config.batch_files)?;
        if page.is_empty() {
            ledger.db.execute(
                "UPDATE scans SET phase='idle',last_completed_ms=?2,after_id=0 WHERE root=?1",
                params![root.to_string_lossy(), now_ms()],
            )?;
        } else {
            for (id, path) in page {
                if Instant::now() >= deadline {
                    break;
                }
                match fs::symlink_metadata(&path) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        ledger.enqueue_scan(&path)?
                    }
                    Err(_) => ledger.enqueue_scan(&path)?,
                    _ => {}
                }
                ledger.db.execute(
                    "UPDATE scans SET after_id=?2 WHERE root=?1",
                    params![root.to_string_lossy(), id],
                )?;
                count += 1;
            }
        }
    }
    transaction.commit()?;
    Ok(count)
}

fn discover(
    workspace: &mut WorkspaceIndex,
    ledger: &Ledger,
    roots: &mut Vec<PathBuf>,
    config: &ProducerConfig,
) -> Result<()> {
    if !config.project_roots.is_empty() {
        let was_dirty = ledger.dirty()?;
        ledger.set("dirty", "1")?;
        let mut changed = false;
        for project in &config.project_roots {
            workspace.register_memory_project(project)?;
            // Git's registry finds newly created linked worktrees without scanning
            // excluded .worktrees trees or unrelated Agent homes.
            let output = std::process::Command::new("git")
                .arg("-C")
                .arg(project)
                .args(["worktree", "list", "--porcelain", "-z"])
                .output()?;
            if output.status.success() {
                for field in output.stdout.split(|b| *b == 0) {
                    if let Some(path) = field.strip_prefix(b"worktree ")
                        && let Ok(path) = std::str::from_utf8(path)
                        && let Ok(path) = fs::canonicalize(path)
                        && !roots.contains(&path)
                    {
                        workspace.register_scan_root(&path)?;
                        roots.push(path);
                    }
                }
            }
        }
        let refresh = workspace.refresh_agent_memories(&config.options)?;
        changed |= refresh.indexed + refresh.deleted > 0;
        if !was_dirty && !changed {
            ledger.set("dirty", "0")?;
        }
    }
    for directory in &config.skill_dirs {
        for entry in WalkBuilder::new(directory)
            .max_depth(Some(4))
            .hidden(false)
            .build()
        {
            let entry = entry?;
            let manifest = if entry.file_name() == "SKILL.md" {
                entry.path().to_owned()
            } else if entry.file_type().is_some_and(|t| t.is_symlink()) {
                // Follow only the manifest in a linked Skill, not its assets or
                // nested links (some distributions contain self-referential links).
                entry.path().join("SKILL.md")
            } else {
                continue;
            };
            if manifest.is_file() {
                let path = fs::canonicalize(manifest)?;
                if !roots.contains(&path) {
                    workspace.register_scan_root(&path)?;
                    roots.push(path);
                }
            }
        }
    }
    Ok(())
}

fn publish(workspace: &mut WorkspaceIndex, ledger: &Ledger, config: &ProducerConfig) -> Result<()> {
    // A failed indexing batch can leave incomplete catalog/derived updates.
    // Seal checks remain mandatory; don't acknowledge its inbox receipt.
    if ledger.dirty()? || !config.publish_dir.join("current.json").exists() {
        workspace.prepare_checkpoint_generation()?;
        ledger.checkpoint(&workspace.index_dir().join("producer-state.sqlite3"))?;
        let manifest = workspace.publish_snapshot(&config.publish_dir)?;
        ledger.set("dirty", "0")?;
        ledger.set("last_published_ms", &now_ms().to_string())?;
        eprintln!("AWI producer published generation {}", manifest.generation);
        prune_generations(&config.publish_dir, config.retain)?;
    }
    ledger.set(
        "snapshot_pointer",
        &serde_json::to_string(&read_pointer(&config.publish_dir)?)?,
    )?;
    // Checkpoint queues and frontier even on no-change scans, without forcing a
    // redundant multi-GB snapshot. It is valid only for the exact sealed pointer.
    let local = workspace.index_dir().join("producer-checkpoint.sqlite3");
    ledger.checkpoint(&local)?;
    let target = config.publish_dir.join("producer-checkpoint.sqlite3");
    let temporary = config
        .publish_dir
        .join(format!(".checkpoint-{}", std::process::id()));
    fs::copy(local, &temporary)?;
    File::open(&temporary)?.sync_all()?;
    fs::rename(temporary, target)?;
    File::open(&config.publish_dir)?.sync_all()?;
    let mut query = ledger.db.prepare("SELECT name FROM receipts")?;
    let names = query
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for name in names {
        match fs::remove_file(config.inbox.join(&name)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        ledger
            .db
            .execute("DELETE FROM receipts WHERE name=?1", [&name])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(temp: &Path, root: &Path) -> ProducerConfig {
        ProducerConfig {
            publish_dir: temp.join("shared"),
            bootstrap_source: None,
            inbox: temp.join("inbox"),
            status_file: temp.join("status"),
            roots: vec![root.to_owned()],
            project_roots: vec![],
            skill_dirs: vec![],
            scan_interval: Duration::from_secs(300),
            memory_interval: Duration::from_secs(60),
            publish_interval: Duration::from_secs(1),
            batch_files: 2,
            batch_budget: Duration::from_secs(1),
            retain: 3,
            options: IndexOptions::default(),
        }
    }

    #[test]
    fn producer_lease_rejects_another_owner() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("producer.lock");
        let owner = lock(&path).unwrap();
        assert!(lock(&path).is_err());
        drop(owner);
        assert!(lock(&path).is_ok());
    }

    #[test]
    fn linked_skill_discovers_manifest_without_following_asset_cycles() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let skills = temp.path().join("skills");
        let target = temp.path().join("actual");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&skills).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("SKILL.md"), "# Linked skill").unwrap();
        std::os::unix::fs::symlink(&target, skills.join("linked")).unwrap();
        std::os::unix::fs::symlink(&target, target.join("cycle")).unwrap();
        let index = temp.path().join("index");
        let mut workspace = WorkspaceIndex::open(&index).unwrap();
        let ledger = Ledger::open(&index.join("producer.sqlite3")).unwrap();
        let mut cfg = config(temp.path(), &root);
        cfg.skill_dirs = vec![skills];
        let mut roots = vec![root];
        discover(&mut workspace, &ledger, &mut roots, &cfg).unwrap();
        assert_eq!(roots.len(), 2);
        assert!(roots.contains(&target.join("SKILL.md")));
    }

    #[test]
    fn failed_publication_keeps_dirty_work_and_expired_batch_keeps_events() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let file = root.join("change.rs");
        fs::write(&file, "fn before() {}").unwrap();
        let index = temp.path().join("index");
        let mut workspace = WorkspaceIndex::open(&index).unwrap();
        workspace
            .index_root(&root, &IndexOptions::default())
            .unwrap();
        let ledger = Ledger::open(&index.join("producer.sqlite3")).unwrap();
        let cfg = config(temp.path(), &root);
        fs::create_dir(&cfg.inbox).unwrap();
        publish(&mut workspace, &ledger, &cfg).unwrap();
        let before = read_pointer(&cfg.publish_dir).unwrap();
        fs::write(&file, "fn after_update() {}").unwrap();
        let event = enqueue(&cfg.inbox, std::slice::from_ref(&file)).unwrap();
        ingest(&ledger, &cfg.inbox).unwrap();
        let (_, attempted) = workspace
            .notify_paths_budgeted(
                std::slice::from_ref(&file),
                &cfg.options,
                Some(Instant::now()),
            )
            .unwrap();
        assert!(attempted.is_empty());
        assert_eq!(ledger.pending().unwrap().0, 1);
        apply_batch(&mut workspace, &ledger, &cfg.roots, &cfg).unwrap();
        let mut bad = cfg.clone();
        bad.publish_dir = temp.path().join("blocked");
        fs::write(&bad.publish_dir, "not a directory").unwrap();
        assert!(publish(&mut workspace, &ledger, &bad).is_err());
        assert!(ledger.dirty().unwrap());
        assert!(event.exists());
        assert_eq!(read_pointer(&cfg.publish_dir).unwrap(), before);
        publish(&mut workspace, &ledger, &cfg).unwrap();
        assert!(!ledger.dirty().unwrap());
        assert!(!event.exists());
        assert_ne!(read_pointer(&cfg.publish_dir).unwrap(), before);
    }

    #[test]
    fn inaccessible_subtree_is_retained_and_scan_stays_incomplete() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        let subtree = root.join("restricted");
        fs::create_dir_all(&subtree).unwrap();
        let file = subtree.join("keep.rs");
        fs::write(&file, "pub fn retain_when_unreachable() {}").unwrap();
        let index = temp.path().join("index");
        let mut workspace = WorkspaceIndex::open(&index).unwrap();
        workspace
            .index_root(&root, &IndexOptions::default())
            .unwrap();
        let ledger = Ledger::open(&index.join("producer.sqlite3")).unwrap();
        let cfg = config(temp.path(), &root);
        fs::set_permissions(&subtree, fs::Permissions::from_mode(0o000)).unwrap();
        // Root CI containers can bypass mode bits; don't misrepresent that as
        // an EACCES test. Non-root CPU-worker tests exercise the full branch.
        let inaccessible = fs::read_dir(&subtree).is_err();
        if inaccessible {
            ledger.schedule(&cfg.roots, 300_000).unwrap();
            let mut active = None;
            for _ in 0..12 {
                scan_batch(&workspace, &ledger, &mut active, &mut None, &cfg).unwrap();
            }
            ledger.enqueue(&file).unwrap();
            apply_batch(&mut workspace, &ledger, &cfg.roots, &cfg).unwrap();
            assert_eq!(ledger.pending().unwrap().0, 1);
            let phase: String = ledger
                .db
                .query_row(
                    "SELECT phase FROM scans WHERE root=?1",
                    [root.to_string_lossy().as_ref()],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(phase, "discovery");
        }
        fs::set_permissions(&subtree, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            workspace
                .search("retain_when_unreachable", 5)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn interrupted_scan_and_published_checkpoint_recover_events() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("workspace");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join(".awiignore"), "ignored/\n").unwrap();
        fs::create_dir(root.join("ignored")).unwrap();
        fs::write(root.join("ignored/secret.rs"), "fn hidden() {}").unwrap();
        for i in 0..8 {
            fs::write(
                root.join(format!("nested/f{i}.rs")),
                format!("pub fn unique{i}() {{}}"),
            )
            .unwrap();
        }
        let index = temp.path().join("index");
        let mut workspace = WorkspaceIndex::open(&index).unwrap();
        workspace.register_scan_root(&root).unwrap();
        let ledger = Ledger::open(&index.join("producer.sqlite3")).unwrap();
        let cfg = config(temp.path(), &root);
        fs::create_dir_all(&cfg.inbox).unwrap();
        ledger.schedule(&cfg.roots, 300_000).unwrap();
        let mut active = None;
        scan_batch(&workspace, &ledger, &mut active, &mut None, &cfg).unwrap();
        // Restart within a directory: rewalk it, not the entire completed frontier.
        active = None;
        for _ in 0..60 {
            scan_batch(&workspace, &ledger, &mut active, &mut None, &cfg).unwrap();
            apply_batch(&mut workspace, &ledger, &cfg.roots, &cfg).unwrap();
        }
        assert_eq!(workspace.search("unique7", 5).unwrap().len(), 1);
        assert!(workspace.search("hidden", 5).unwrap().is_empty());
        assert!(!allowed(&root, &root.join("ignored/secret.rs")));
        let event = enqueue(&cfg.inbox, &[root.join("nested/f0.rs")]).unwrap();
        ingest(&ledger, &cfg.inbox).unwrap();
        ingest(&ledger, &cfg.inbox).unwrap();
        assert_eq!(ledger.pending().unwrap().0, 1);
        // Publish with pending event: its queue is checkpointed before the shared
        // event can be removed. A replacement worker must recover that queue.
        publish(&mut workspace, &ledger, &cfg).unwrap();
        assert!(!event.exists());
        let restored = temp.path().join("replacement");
        bootstrap(&restored, &cfg.publish_dir).unwrap();
        let recovery = Ledger::open(&restored.join("producer.sqlite3")).unwrap();
        assert_eq!(recovery.pending().unwrap().0, 1);
        fs::remove_dir_all(root.join("nested")).unwrap();
        assert_eq!(
            notification_path(&root.join("nested/f0.rs")).unwrap(),
            root.join("nested/f0.rs")
        );
        workspace
            .notify_paths(&[root.join("nested/f0.rs")], &cfg.options)
            .unwrap();
        assert!(workspace.search("unique0", 5).unwrap().is_empty());
    }
}
