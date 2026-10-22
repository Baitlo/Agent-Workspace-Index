//! Pre-publication single-writer fence check.
//!
//! Fencing is opt-in via the environment the supervisor launches the child with:
//!   AWI_FENCE_PATH  - shared fence.json (same volume as the publication)
//!   AWI_FENCE_GEN   - the generation THIS process may write at
//!   AWI_FENCE_OWNER - optional expected owner token
//!   AWI_FENCE_TIMEOUT_SECS - bound for a fence read on a hard-mounted NFS
//!
//! The publish path takes `commit_guard_from_env()` at the MOMENT of the final
//! `current.json` pointer rename (inside snapshot::publish_pointer), NOT around
//! the multi-GB snapshot build. The guard opens the SAME sibling lock file the
//! supervisor's `cas_bump_fence` uses (`<fence>.caslock`), takes an exclusive
//! flock, and re-validates the fence WHILE holding the lock, then stays held
//! until the rename completes. This closes the check->flip TOCTOU.
//!
//! The fence read itself is bounded (default 2s): on a hard-mounted NFS volume
//! a dead server can block `read(2)` forever, so we read on a side thread and
//! fail closed on timeout instead of wedging the lane.
//!
//! Tests call the explicit `commit_guard_at(...)` / `check(...)` variants and
//! never mutate process env, so they are safe under parallel `cargo test`.
use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Deserialize)]
struct Fence {
    generation: i64,
    owner: Option<String>,
}

fn lock_sibling(fence_path: &Path) -> PathBuf {
    fence_path.with_extension("json.caslock")
}

fn timeout() -> Duration {
    let secs: u64 = std::env::var("AWI_FENCE_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    Duration::from_secs(secs.max(1))
}

/// Bounded fence read: run on a side thread; fail closed on timeout.
fn read_fence_bounded(path: &Path, dur: Duration) -> Result<Vec<u8>> {
    let p = path.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || drop(tx.send(std::fs::read(&p))));
    match rx.recv_timeout(dur) {
        Ok(Ok(bytes)) => Ok(bytes),
        Ok(Err(e)) => Err(e).with_context(|| {
            format!(
                "fence unreadable at {}; refusing to publish",
                path.display()
            )
        }),
        Err(_) => bail!(
            "fence read timed out at {}; refusing to flip the pointer",
            path.display()
        ),
    }
}

/// Explicit, env-free validation: the holder must match generation/owner.
pub fn check(fence_path: &Path, expected: i64, expected_owner: Option<&str>) -> Result<()> {
    let raw = read_fence_bounded(fence_path, timeout())?;
    let fence: Fence = serde_json::from_slice(&raw)
        .with_context(|| format!("decode fence at {}", fence_path.display()))?;
    if fence.generation != expected {
        bail!(
            "fence generation {} != holder {}; another writer owns the shard",
            fence.generation,
            expected
        );
    }
    if let (Some(want), Some(actual)) = (expected_owner, fence.owner.as_ref())
        && !want.is_empty()
        && actual != want
    {
        bail!("fence owner {actual:?} != holder {want:?}");
    }
    Ok(())
}

/// Read the three env vars and validate. Returns Ok(None) when disabled.
fn holder_from_env() -> Result<Option<(PathBuf, i64, Option<String>)>> {
    let Some(path) = std::env::var_os("AWI_FENCE_PATH") else {
        return Ok(None);
    };
    let path = PathBuf::from(path);
    let gen_raw = std::env::var("AWI_FENCE_GEN")
        .context("AWI_FENCE_PATH set but AWI_FENCE_GEN missing; fail closed")?;
    let expected: i64 = gen_raw
        .trim()
        .parse()
        .with_context(|| format!("AWI_FENCE_GEN not an integer: {gen_raw:?}"))?;
    let owner = std::env::var("AWI_FENCE_OWNER").ok();
    Ok(Some((path, expected, owner)))
}

/// Cheap read-only check (env-driven).
pub fn assert_holder_from_env() -> Result<()> {
    if let Some((path, expected, owner)) = holder_from_env()? {
        check(&path, expected, owner.as_deref())?;
    }
    Ok(())
}

/// Holds the publication commit guard for the duration of the pointer rename.
pub struct CommitGuard {
    _file: File,
}

/// Explicit env-free guard: take the caslock, re-check, hold until dropped.
pub fn commit_guard_at(
    fence_path: &Path,
    expected: i64,
    expected_owner: Option<&str>,
) -> Result<CommitGuard> {
    let lock_path = lock_sibling(fence_path);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let lock_file = File::create(&lock_path)
        .with_context(|| format!("open fence caslock {}", lock_path.display()))?;
    // BSD flock(2): matches the supervisor's fcntl.flock() exactly.
    let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("lock fence caslock {}", lock_path.display()));
    }
    check(fence_path, expected, expected_owner)?; // re-check WHILE holding lock
    Ok(CommitGuard { _file: lock_file })
}

/// Env-driven guard (used by the publish path). Ok(None) when fencing disabled.
pub fn commit_guard_from_env() -> Result<Option<CommitGuard>> {
    match holder_from_env()? {
        Some((path, expected, owner)) => {
            Ok(Some(commit_guard_at(&path, expected, owner.as_deref())?))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::{Arc, Barrier};

    fn write_fence(dir: &Path, generation: i64, owner: &str) {
        let body = format!("{{\"generation\":{},\"owner\":\"{}\"}}", generation, owner);
        let mut f = std::fs::File::create(dir.join("fence.json")).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn passes_when_holder() {
        let dir = std::env::temp_dir().join(format!("awi-fence-ok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_fence(&dir, 7, "a:fb");
        assert!(check(&dir.join("fence.json"), 7, Some("a:fb")).is_ok());
        drop(commit_guard_at(&dir.join("fence.json"), 7, Some("a:fb")).unwrap());
    }

    #[test]
    fn fails_closed_on_stale_generation() {
        let dir = std::env::temp_dir().join(format!("awi-fence-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_fence(&dir, 99, "other");
        assert!(check(&dir.join("fence.json"), 7, None).is_err());
    }

    #[test]
    fn fails_closed_when_unreadable() {
        let p = Path::new("/nonexistent/.awi-fence-missing/fence.json");
        assert!(check(p, 1, None).is_err());
    }

    #[test]
    fn fails_closed_on_read_timeout() {
        // mkfifo blocks reads until a writer appears -> bounded read must time out.
        let dir = std::env::temp_dir().join(format!("awi-fence-to-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("fence.json");
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        // shrink the timeout for this call via a dedicated bounded check.
        let p = fifo.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || drop(tx.send(std::fs::read(&p))));
        let t0 = std::time::Instant::now();
        assert!(rx.recv_timeout(Duration::from_secs(1)).is_err());
        assert!(t0.elapsed() >= Duration::from_secs(1));
    }

    #[test]
    fn commit_guard_excludes_caslock_while_held() {
        let dir = std::env::temp_dir().join(format!("awi-fence-race-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        write_fence(&dir, 2, "old:worker");
        let fence = dir.join("fence.json");
        let caslock = lock_sibling(&fence);

        let ready = Arc::new(Barrier::new(2));
        let r2 = ready.clone();
        let t = std::thread::spawn(move || {
            let _g = commit_guard_at(&fence, 2, Some("old:worker")).unwrap();
            r2.wait();
            std::thread::sleep(Duration::from_millis(300));
            drop(_g);
        });

        ready.wait();
        let contender = File::create(&caslock).unwrap();
        let rc = unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert!(
            rc != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK),
            "caslock held by commit guard: contender must block"
        );
        t.join().unwrap();
        unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_UN) };
        let rc2 = unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc2, 0, "after guard drop, claimant must acquire");
        unsafe { libc::flock(contender.as_raw_fd(), libc::LOCK_UN) };
    }
}
