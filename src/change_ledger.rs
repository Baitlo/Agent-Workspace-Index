//! Local WAL ledger. Shared storage receives only closed, atomic checkpoints.
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

pub(crate) struct Ledger {
    pub db: Connection,
}

#[derive(Debug)]
pub(crate) struct Change {
    pub path: PathBuf,
    pub revision: i64,
    pub attempts: u32,
}

#[derive(Serialize)]
pub(crate) struct ScanStatus {
    root: String,
    phase: String,
    last_completed_ms: i64,
    after_id: i64,
    directories: u64,
}

impl Ledger {
    pub fn open(path: &Path) -> Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=FULL;
             CREATE TABLE IF NOT EXISTS changes (
                path TEXT PRIMARY KEY, revision INTEGER NOT NULL,
                received_ms INTEGER NOT NULL, attempts INTEGER NOT NULL DEFAULT 0,
                retry_ms INTEGER NOT NULL DEFAULT 0, error TEXT,
                priority INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT OR IGNORE INTO state VALUES ('dirty', '0');
             CREATE TABLE IF NOT EXISTS receipts (name TEXT PRIMARY KEY);
             CREATE TABLE IF NOT EXISTS scans (
                root TEXT PRIMARY KEY, phase TEXT NOT NULL DEFAULT 'idle',
                last_completed_ms INTEGER NOT NULL DEFAULT 0,
                after_id INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS directories (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                root TEXT NOT NULL, path TEXT NOT NULL, retry_ms INTEGER NOT NULL DEFAULT 0,
                error TEXT, UNIQUE(root, path));
             CREATE INDEX IF NOT EXISTS directories_ready ON directories(retry_ms,id);",
        )?;
        Ok(Self { db })
    }

    pub fn enqueue(&self, path: &Path) -> Result<()> {
        self.enqueue_priority(path, 1)
    }

    pub fn enqueue_scan(&self, path: &Path) -> Result<()> {
        self.enqueue_priority(path, 0)
    }

    fn enqueue_priority(&self, path: &Path, priority: i32) -> Result<()> {
        self.db.execute(
            "INSERT INTO changes(path,revision,received_ms,priority) VALUES (?1,1,?2,?3)
             ON CONFLICT(path) DO UPDATE SET revision=revision+1, retry_ms=0,
                attempts=0, error=NULL, priority=MAX(priority,excluded.priority)",
            params![path.to_string_lossy(), now_ms(), priority],
        )?;
        Ok(())
    }

    pub fn batch(&self, limit: usize) -> Result<Vec<Change>> {
        let mut query = self.db.prepare(
            "SELECT path,revision,attempts FROM changes WHERE retry_ms<=?1
             ORDER BY priority DESC,received_ms,path LIMIT ?2",
        )?;
        Ok(query
            .query_map(params![now_ms(), limit as i64], |row| {
                Ok(Change {
                    path: PathBuf::from(row.get::<_, String>(0)?),
                    revision: row.get(1)?,
                    attempts: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn ack(&self, change: &Change) -> Result<()> {
        self.db.execute(
            "DELETE FROM changes WHERE path=?1 AND revision=?2",
            params![change.path.to_string_lossy(), change.revision],
        )?;
        Ok(())
    }

    pub fn retry(&self, change: &Change, error: &str) -> Result<()> {
        let delay = 1_000_i64 * (1_i64 << change.attempts.min(8));
        self.db.execute(
            "UPDATE changes SET attempts=attempts+1,retry_ms=?3,error=?4
             WHERE path=?1 AND revision=?2",
            params![
                change.path.to_string_lossy(),
                change.revision,
                now_ms() + delay,
                error
            ],
        )?;
        Ok(())
    }

    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO state VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [key, value],
        )?;
        Ok(())
    }

    pub fn get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM state WHERE key=?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn dirty(&self) -> Result<bool> {
        Ok(self.get("dirty")?.as_deref() == Some("1"))
    }

    pub fn directory(&self, root: &Path, path: &Path) -> Result<()> {
        self.db.execute(
            "INSERT OR IGNORE INTO directories(root,path) VALUES (?1,?2)",
            params![root.to_string_lossy(), path.to_string_lossy()],
        )?;
        self.db.execute(
            "UPDATE scans SET phase='discovery' WHERE root=?1",
            [root.to_string_lossy().as_ref()],
        )?;
        Ok(())
    }

    pub fn schedule(&self, roots: &[PathBuf], interval_ms: i64) -> Result<()> {
        for root in roots {
            self.db.execute(
                "INSERT OR IGNORE INTO scans(root) VALUES (?1)",
                [root.to_string_lossy().as_ref()],
            )?;
            let due: bool = self.db.query_row(
                "SELECT phase='idle' AND last_completed_ms<=?2 FROM scans WHERE root=?1",
                params![root.to_string_lossy(), now_ms() - interval_ms],
                |r| r.get(0),
            )?;
            if due {
                self.directory(root, root)?;
                self.db.execute(
                    "UPDATE scans SET after_id=0 WHERE root=?1",
                    [root.to_string_lossy().as_ref()],
                )?;
            }
        }
        Ok(())
    }

    pub fn pending(&self) -> Result<(u64, Option<i64>)> {
        Ok(self
            .db
            .query_row("SELECT COUNT(*),MIN(received_ms) FROM changes", [], |r| {
                Ok((r.get::<_, i64>(0)? as u64, r.get(1)?))
            })?)
    }

    pub fn ready_count(&self) -> Result<i64> {
        Ok(self.db.query_row(
            "SELECT COUNT(*) FROM changes WHERE retry_ms<=?1",
            [now_ms()],
            |r| r.get(0),
        )?)
    }

    pub fn scans(&self) -> Result<Vec<ScanStatus>> {
        let mut query = self.db.prepare(
            "SELECT root,phase,last_completed_ms,after_id,
             (SELECT COUNT(*) FROM directories d WHERE d.root=s.root) FROM scans s ORDER BY root",
        )?;
        Ok(query
            .query_map([], |r| {
                Ok(ScanStatus {
                    root: r.get(0)?,
                    phase: r.get(1)?,
                    last_completed_ms: r.get(2)?,
                    after_id: r.get(3)?,
                    directories: r.get::<_, i64>(4)? as u64,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn roots(&self) -> Result<Vec<PathBuf>> {
        let mut statement = self.db.prepare("SELECT root FROM scans ORDER BY root")?;
        Ok(statement
            .query_map([], |r| Ok(PathBuf::from(r.get::<_, String>(0)?)))?
            .collect::<rusqlite::Result<_>>()?)
    }

    pub fn checkpoint(&self, path: &Path) -> Result<()> {
        let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
        if temporary.exists() {
            fs::remove_file(&temporary)?;
        }
        self.db
            .execute("VACUUM INTO ?1", [temporary.to_string_lossy().as_ref()])?;
        fs::File::open(&temporary)?.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(path.parent().expect("checkpoint parent"))?.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revisions_retries_and_crash_replay() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("ledger");
        let file = Path::new("/workspace/new.rs");
        let ledger = Ledger::open(&path).unwrap();
        ledger.enqueue(file).unwrap();
        let old = ledger.batch(1).unwrap().remove(0);
        ledger.enqueue(file).unwrap();
        ledger.ack(&old).unwrap();
        assert_eq!(ledger.pending().unwrap().0, 1);
        ledger.set("dirty", "1").unwrap();
        drop(ledger);
        let ledger = Ledger::open(&path).unwrap();
        assert!(ledger.dirty().unwrap());
        let new = ledger.batch(1).unwrap().remove(0);
        assert_eq!(new.revision, 2);
        ledger.retry(&new, "transient").unwrap();
        assert!(ledger.batch(1).unwrap().is_empty());
        assert_eq!(ledger.pending().unwrap().0, 1);
        let checkpoint = temp.path().join("checkpoint");
        ledger.checkpoint(&checkpoint).unwrap();
        let restored = Ledger::open(&checkpoint).unwrap();
        assert_eq!(restored.pending().unwrap().0, 1);
        assert!(restored.dirty().unwrap());
    }
}
