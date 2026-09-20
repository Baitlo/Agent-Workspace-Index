use std::fs;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use awi::{IndexOptions, WorkspaceIndex};
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn indexes_code_text_and_tabular_metadata_incrementally() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    let index_dir = fixture.path().join("index");
    fs::create_dir_all(root.join("experiment_v1/src")).unwrap();

    let source = root.join("experiment_v1/src/lib.rs");
    let sql = root.join("experiment_v1/report.sql");
    let csv = root.join("experiment_v1/metrics.csv");
    fs::write(
        &source,
        "pub fn build_workspace_index() -> usize { helper() }\nfn helper() -> usize { 1 }\n",
    )
    .unwrap();
    fs::write(
        &sql,
        "SELECT campaign_id, SUM(spend) AS total_spend FROM ad_cost GROUP BY campaign_id;\n",
    )
    .unwrap();
    fs::write(&csv, "model,quality_score\nawi,0.95\n").unwrap();
    fs::write(root.join(".env"), "SECRET=not-indexed\n").unwrap();

    let mut workspace = WorkspaceIndex::open(&index_dir).unwrap();
    let first = workspace
        .index_root(&root, &IndexOptions::default())
        .unwrap();
    assert_eq!(first.discovered, 3);
    assert_eq!(first.indexed, 3);
    assert_eq!(first.failed, 0);

    let symbol_hits = workspace.search("build_workspace_index", 10).unwrap();
    assert!(symbol_hits.iter().any(|hit| {
        hit.path.ends_with("lib.rs") && hit.matched_lanes.iter().any(|lane| lane == "symbol")
    }));

    let text_hits = workspace.search("total_spend", 10).unwrap();
    assert!(text_hits.iter().any(|hit| hit.path.ends_with("report.sql")));

    let inspected = workspace.inspect(&source).unwrap();
    assert!(
        inspected
            .symbols
            .iter()
            .any(|symbol| { symbol.name == "build_workspace_index" && symbol.kind == "function" })
    );
    assert!(
        inspected
            .content
            .as_ref()
            .unwrap()
            .text
            .contains("build_workspace_index")
    );
    let excerpt = workspace.inspect_excerpt(&source, 2, 1, 10).unwrap();
    let excerpt = excerpt.content.unwrap();
    assert_eq!(excerpt.start_line, 2);
    assert_eq!(excerpt.end_line, 2);
    assert!(excerpt.truncated);
    assert!(excerpt.text.chars().count() <= 10);

    let schema_hits = workspace.search("quality_score", 10).unwrap();
    assert!(schema_hits.iter().any(|hit| {
        hit.path.ends_with("metrics.csv") && hit.matched_lanes.iter().any(|lane| lane == "schema")
    }));
    let inspected_csv = workspace.inspect(&csv).unwrap();
    assert_eq!(
        inspected_csv
            .dataset
            .as_ref()
            .map(|value| value.status.as_str()),
        Some("profiled")
    );

    let second = workspace
        .index_root(&root, &IndexOptions::default())
        .unwrap();
    assert_eq!(second.indexed, 0);
    assert_eq!(second.unchanged, 3);

    fs::write(
        &sql,
        "SELECT campaign_id, SUM(clicks) AS total_clicks FROM ad_cost GROUP BY campaign_id;\n",
    )
    .unwrap();
    let third = workspace
        .index_root(&root, &IndexOptions::default())
        .unwrap();
    assert_eq!(third.indexed, 1);
    assert_eq!(third.unchanged, 2);
    assert!(
        workspace
            .search("total_clicks", 10)
            .unwrap()
            .iter()
            .any(|hit| hit.path.ends_with("report.sql"))
    );
    assert!(
        !workspace
            .search("total_spend", 10)
            .unwrap()
            .iter()
            .any(|hit| hit.path.ends_with("report.sql"))
    );

    fs::remove_file(&csv).unwrap();
    let fourth = workspace
        .index_root(&root, &IndexOptions::default())
        .unwrap();
    assert_eq!(fourth.deleted, 1);
    assert!(
        !workspace
            .search("quality_score", 10)
            .unwrap()
            .iter()
            .any(|hit| hit.path.ends_with("metrics.csv"))
    );
}

const START_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn daemon_serves_cli_requests_and_stops_cleanly() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("daemon-workspace");
    let index_dir = fixture.path().join("daemon-index");
    let socket = fixture.path().join("awi.sock");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("service.rs"),
        "pub fn daemon_search_target() -> usize { 1 }\n",
    )
    .unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args(["--index-dir", path(&index_dir), "--socket", path(&socket)])
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = ChildGuard::new(child);

    wait_until_ready(&mut daemon, &index_dir, &socket);

    let ping = run_json(&index_dir, &socket, &["ping", "--json"]);
    assert_eq!(ping["service"], "awi");
    assert_eq!(ping["protocol_version"], 3);

    let report = run_json(&index_dir, &socket, &["index", path(&root), "--json"]);
    assert_eq!(report["indexed"], 1);
    assert_eq!(report["failed"], 0);

    let hits = run_json(
        &index_dir,
        &socket,
        &["search", "daemon_search_target", "--limit", "5", "--json"],
    );
    assert!(
        hits.as_array()
            .unwrap()
            .iter()
            .any(|hit| hit["path"].as_str().unwrap().ends_with("service.rs"))
    );

    let status = run_json(&index_dir, &socket, &["status", "--json"]);
    assert_eq!(status["active_files"], 1);
    assert_eq!(status["running_generations"], 0);

    fs::write(
        root.join("service.rs"),
        "pub fn daemon_notify_target() -> usize { 2 }\n",
    )
    .unwrap();
    let notified = run_json(
        &index_dir,
        &socket,
        &["notify", path(&root.join("service.rs")), "--json"],
    );
    assert_eq!(notified["roots"][0]["indexed"], 1);
    let notified_hits = run_json(
        &index_dir,
        &socket,
        &["search", "daemon_notify_target", "--json"],
    );
    assert_eq!(notified_hits.as_array().unwrap().len(), 1);

    fs::remove_file(root.join("service.rs")).unwrap();
    let deleted = run_json(
        &index_dir,
        &socket,
        &["notify", path(&root.join("service.rs")), "--json"],
    );
    assert_eq!(deleted["roots"][0]["deleted"], 1);
    let deleted_hits = run_json(
        &index_dir,
        &socket,
        &["search", "daemon_notify_target", "--json"],
    );
    assert!(deleted_hits.as_array().unwrap().is_empty());

    let stopped = run_json(&index_dir, &socket, &["stop", "--json"]);
    assert_eq!(stopped["stopping"], true);
    wait_until_stopped(&mut daemon);
    assert!(!socket.exists());
}

#[test]
fn daemon_atomically_switches_published_snapshots() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    let writer_index = fixture.path().join("writer-index");
    let publish_dir = fixture.path().join("published");
    let runtime_index = fixture.path().join("runtime-index");
    let socket = fixture.path().join("snapshot.sock");
    let source = root.join("service.rs");
    fs::create_dir_all(&root).unwrap();
    fs::write(&source, "pub fn snapshot_version_one() -> usize { 1 }\n").unwrap();

    {
        let mut workspace = WorkspaceIndex::open(&writer_index).unwrap();
        workspace
            .index_root(&root, &IndexOptions::default())
            .unwrap();
        let manifest = workspace.publish_snapshot(&publish_dir).unwrap();
        assert_eq!(manifest.generation, 1);
    }

    let child = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args([
            "--index-dir",
            path(&runtime_index),
            "--socket",
            path(&socket),
        ])
        .arg("serve")
        .args([
            "--snapshot-source",
            path(&publish_dir),
            "--snapshot-poll-ms",
            "0",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = ChildGuard::new(child);
    wait_until_ready(&mut daemon, &runtime_index, &socket);
    let first = run_json(
        &runtime_index,
        &socket,
        &["search", "snapshot_version_one", "--json"],
    );
    assert_eq!(first.as_array().unwrap().len(), 1);

    fs::write(&source, "pub fn snapshot_version_two() -> usize { 2 }\n").unwrap();
    {
        let mut workspace = WorkspaceIndex::open(&writer_index).unwrap();
        workspace
            .index_root(&root, &IndexOptions::default())
            .unwrap();
        let manifest = workspace.publish_snapshot(&publish_dir).unwrap();
        assert_eq!(manifest.generation, 2);
    }

    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        let hits = run_json(
            &runtime_index,
            &socket,
            &["search", "snapshot_version_two", "--json"],
        );
        if hits.as_array().is_some_and(|hits| !hits.is_empty()) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not activate the new snapshot"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let stale = run_json(
        &runtime_index,
        &socket,
        &["search", "snapshot_version_one", "--json"],
    );
    assert!(stale.as_array().unwrap().is_empty());

    run_json(&runtime_index, &socket, &["stop", "--json"]);
    wait_until_stopped(&mut daemon);
}

fn path(value: &Path) -> &str {
    value.to_str().unwrap()
}

fn daemon_command(index_dir: &Path, socket: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_awi"));
    command.args(["--index-dir", path(index_dir), "--socket", path(socket)]);
    command
}

fn run_json(index_dir: &Path, socket: &Path, args: &[&str]) -> Value {
    let output = daemon_command(index_dir, socket)
        .args(args)
        .output()
        .unwrap();
    assert_success(&output);
    serde_json::from_slice(&output.stdout).unwrap()
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "command failed: status={:?}, stdout={}, stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn wait_until_ready(daemon: &mut ChildGuard, index_dir: &Path, socket: &Path) {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            panic!("AWI daemon exited before becoming ready: {status}");
        }
        if daemon_command(index_dir, socket)
            .args(["ping", "--json"])
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "AWI daemon did not become ready within {START_TIMEOUT:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_until_stopped(daemon: &mut ChildGuard) {
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        if let Some(status) = daemon.child.try_wait().unwrap() {
            assert!(status.success(), "AWI daemon exited with {status}");
            daemon.reaped = true;
            return;
        }
        assert!(
            Instant::now() < deadline,
            "AWI daemon did not stop within {STOP_TIMEOUT:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

struct ChildGuard {
    child: Child,
    reaped: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
