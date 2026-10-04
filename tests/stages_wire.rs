//! End-to-end regression: a real `awi serve` daemon carries per-query stage
//! diagnostics on the wire, warm reader reuse never replays a snapshot-open
//! duration, and the aggregate serving status reads the shared worker metrics.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use tempfile::tempdir;

fn path(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_ready(socket: &Path) {
    for _ in 0..100 {
        if UnixStream::connect(socket).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("daemon socket never appeared at {}", socket.display());
}

/// Send a raw request envelope and return the raw response envelope JSON.
fn raw_envelope(socket: &Path, request: &Value) -> Value {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream
        .write_all(
            serde_json::to_string(&serde_json::json!({
                "protocol_version": 3,
                "request": request,
            }))
            .unwrap()
            .as_bytes(),
        )
        .unwrap();
    stream.write_all(b"\n").unwrap();
    stream.flush().unwrap();
    let mut line = String::new();
    stream.read_to_string(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn cli(index_dir: &Path, socket: &Path, args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args([
            "--index-dir",
            path(index_dir).as_str(),
            "--socket",
            path(socket).as_str(),
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "cli {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

#[test]
fn daemon_wire_carries_five_stage_diagnostics_and_warm_reader_no_replay() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    let index_dir = fixture.path().join("index");
    let socket = fixture.path().join("awi.sock");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("lib.rs"),
        "pub fn wire_stage_target() -> usize { 42 }\n",
    )
    .unwrap();

    let child = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args([
            "--index-dir",
            path(&index_dir).as_str(),
            "--socket",
            path(&socket).as_str(),
        ])
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = ChildGuard(child);
    wait_ready(&socket);

    let report = cli(
        &index_dir,
        &socket,
        &["index", path(&root).as_str(), "--json"],
    );
    assert_eq!(report["indexed"], 1);

    let search_req = serde_json::json!({
        "method": "search",
        "params": {"query": "wire_stage_target", "limit": 5, "roots": [], "kinds": []}
    });

    // First real query: stages must be present with real values.
    let first = raw_envelope(&socket, &search_req);
    assert!(first["ok"].as_bool().unwrap());
    let stages = &first["stages"];
    assert!(
        stages["queue_wait_ms"].is_number(),
        "queue_wait_ms must be a real number: {stages}"
    );
    // No rotation open on the first query after worker start: warm reader reused.
    assert!(
        stages["reader_snapshot_open_ms"].is_null(),
        "warm reader must not replay open duration: {stages}"
    );
    assert_eq!(stages["reader_snapshot_reopened"], false);
    assert!(stages["sidecar_startup_ms"].is_null());
    assert_eq!(stages["sidecar_startup_state"], "warm");

    // Second identical query: still no fabricated open, stages still present.
    let second = raw_envelope(&socket, &search_req);
    let stages2 = &second["stages"];
    assert!(stages2["queue_wait_ms"].is_number());
    assert!(
        stages2["reader_snapshot_open_ms"].is_null(),
        "cache/warm path must not replay old open: {stages2}"
    );

    // Aggregate status reads the shared worker metrics: the worker startup open
    // must have produced a real reader_snapshot_open sample.
    let status = cli(&index_dir, &socket, &["status", "--json"]);
    let samples = status["serving"]["reader_snapshot_open"]["samples"]
        .as_u64()
        .expect("serving.reader_snapshot_open.samples must exist from shared metrics");
    assert!(
        samples >= 1,
        "worker startup open must record a shared sample: {status}"
    );

    let _ = daemon.0.kill();
}
