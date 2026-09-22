use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use awi::{IndexOptions, WorkspaceIndex};
use serde_json::{Value, json};
use tempfile::tempdir;

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn mcp_stdio_exposes_search_inspect_and_query() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    let index_dir = fixture.path().join("index");
    let socket = fixture.path().join("missing-daemon.sock");
    let audit_log = fixture.path().join("mcp-calls.jsonl");
    let source = root.join("service.rs");
    let metrics = root.join("metrics.csv");
    let instructions = root.join("AGENTS.md");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        &source,
        "pub fn mcp_search_target() -> usize { helper() }\nfn helper() -> usize { 1 }\n",
    )
    .unwrap();
    fs::write(&metrics, "model,score\nawi,0.95\nbaseline,0.80\n").unwrap();
    fs::write(
        &instructions,
        "# Workspace Policy\nUse MCP evidence before deployment.\n",
    )
    .unwrap();

    {
        let mut workspace = WorkspaceIndex::open(&index_dir).unwrap();
        let report = workspace
            .index_root(&root, &IndexOptions::default())
            .unwrap();
        assert_eq!(report.indexed, 3);
        assert_eq!(report.failed, 0);
    }

    let mut mcp = McpProcess::spawn(&index_dir, &socket, &audit_log);
    let initialized = mcp.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {
                "name": "awi-integration-test",
                "version": "0.1.0"
            }
        }),
    );
    assert_eq!(initialized["result"]["serverInfo"]["name"], "awi");
    assert!(
        initialized["result"]["capabilities"]["tools"]
            .as_object()
            .is_some()
    );
    mcp.notify("notifications/initialized", json!({}));

    let listed = mcp.request(2, "tools/list", json!({}));
    let tools = listed["result"]["tools"].as_array().unwrap();
    let mut names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(
        names,
        ["workspace_inspect", "workspace_query", "workspace_search"]
    );
    assert!(
        tools
            .iter()
            .all(|tool| tool["annotations"]["readOnlyHint"] == true)
    );
    let search_tool = tools
        .iter()
        .find(|tool| tool["name"] == "workspace_search")
        .unwrap();
    assert_eq!(
        search_tool["inputSchema"]["properties"]["limit"]["minimum"],
        1
    );
    assert_eq!(
        search_tool["inputSchema"]["properties"]["limit"]["maximum"],
        50
    );
    assert!(
        search_tool["inputSchema"]["properties"]["context_path"]
            .as_object()
            .is_some()
    );

    let searched = mcp.request(
        3,
        "tools/call",
        json!({
            "name": "workspace_search",
            "arguments": {
                "query": "mcp_search_target",
                "limit": 5,
                "roots": [root.clone()],
                "kinds": ["source"],
                "path_prefix": "service"
            }
        }),
    );
    let hits = searched["result"]["structuredContent"]["hits"]
        .as_array()
        .unwrap();
    assert_eq!(
        searched["result"]["structuredContent"]["format"],
        "compact_v2"
    );
    assert_eq!(
        searched["result"]["structuredContent"]["requested_limit"],
        5
    );
    assert_eq!(
        searched["result"]["structuredContent"]["effective_limit"],
        5
    );
    assert!(
        hits.iter()
            .any(|hit| hit["path"].as_str().unwrap().ends_with("service.rs"))
    );
    assert!(hits.iter().all(|hit| hit.get("file_id").is_none()));
    assert!(hits.iter().all(|hit| hit.get("size_bytes").is_none()));

    let agent_search = mcp.request(
        31,
        "tools/call",
        json!({
            "name": "workspace_search",
            "arguments": {
                "query": "MCP evidence deployment",
                "limit": 5,
                "kinds": ["agent_instructions"],
                "context_path": source
            }
        }),
    );
    let agent_hits = agent_search["result"]["structuredContent"]["hits"]
        .as_array()
        .unwrap();
    assert_eq!(agent_hits.len(), 1);
    assert_eq!(agent_hits[0]["kind"], "agent_instructions");
    assert_eq!(agent_hits[0]["agent"]["role"], "instructions");

    let parent_scoped = mcp.request(
        30,
        "tools/call",
        json!({
            "name": "workspace_search",
            "arguments": {
                "query": "mcp_search_target",
                "limit": 5,
                "roots": [fixture.path()]
            }
        }),
    );
    assert_eq!(
        parent_scoped["result"]["structuredContent"]["hits"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let inspected = mcp.request(
        4,
        "tools/call",
        json!({
            "name": "workspace_inspect",
            "arguments": {
                "path": source,
                "max_symbols": 10
            }
        }),
    );
    let inspected = &inspected["result"]["structuredContent"];
    assert!(
        inspected["file"]["absolute_path"]
            .as_str()
            .unwrap()
            .ends_with("service.rs")
    );
    assert!(
        inspected["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .any(|symbol| symbol["name"] == "mcp_search_target")
    );
    assert!(
        inspected["content"]["text"]
            .as_str()
            .unwrap()
            .contains("mcp_search_target")
    );
    assert_eq!(inspected["coverage"]["symbols_truncated"], false);

    let outside = fixture.path().join("outside");
    let secret = outside.join("secret.json");
    fs::create_dir_all(&outside).unwrap();
    fs::write(&secret, "{\"secret\":true}\n").unwrap();

    let queried = mcp.request(
        5,
        "tools/call",
        json!({
            "name": "workspace_query",
            "arguments": {
                "sql": "SELECT model, score FROM metrics ORDER BY score DESC",
                "roots": [root.clone()],
                "inputs": [{"path": metrics, "alias": "metrics"}],
                "max_rows": 1
            }
        }),
    );
    let queried = &queried["result"]["structuredContent"];
    assert_eq!(queried["row_count"], 1);
    assert_eq!(queried["rows"][0][0], "awi");
    assert_eq!(queried["truncated"], true);

    let escaped = mcp.request(
        6,
        "tools/call",
        json!({
            "name": "workspace_query",
            "arguments": {
                "sql": "SELECT * FROM secret",
                "roots": [outside],
                "inputs": [{"path": secret, "alias": "secret"}]
            }
        }),
    );
    assert_eq!(escaped["result"]["isError"], true);
    assert_eq!(
        escaped["result"]["structuredContent"]["error"]["code"],
        "query_failed"
    );
    assert!(
        escaped["result"]["structuredContent"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not an indexed workspace root")
    );
    assert!(
        escaped["result"]["structuredContent"]["error"]["recovery"]
            .as_str()
            .unwrap()
            .contains("exact registered root")
    );

    let invalid = mcp.request(
        7,
        "tools/call",
        json!({
            "name": "workspace_search",
            "arguments": {
                "query": "target",
                "limit": 51
            }
        }),
    );
    assert_eq!(invalid["error"]["code"], -32602);

    mcp.close();

    let records = fs::read_to_string(&audit_log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 7);
    assert_eq!(records[0]["tool"], "workspace_search");
    assert_eq!(records[0]["status"], "ok");
    assert_eq!(records[0]["result_count_kind"], "hits");
    assert_eq!(records[0]["result_count"], 1);
    assert_eq!(records[1]["tool"], "workspace_search");
    assert_eq!(records[1]["status"], "ok");
    assert_eq!(
        records[1]["arguments"]["context_path"].as_str(),
        source.to_str()
    );
    assert_eq!(records[2]["tool"], "workspace_search");
    assert_eq!(records[2]["status"], "ok");
    assert_eq!(
        records[2]["arguments"]["roots"][0].as_str(),
        fixture.path().to_str()
    );
    assert_eq!(records[3]["tool"], "workspace_inspect");
    assert_eq!(records[3]["status"], "ok");
    assert_eq!(records[4]["tool"], "workspace_query");
    assert_eq!(records[4]["status"], "ok");
    assert_eq!(records[4]["result_count_kind"], "rows");
    assert_eq!(records[4]["result_count"], 1);
    assert_eq!(records[5]["status"], "tool_error");
    assert_eq!(records[5]["error_code"], "query_failed");
    assert_eq!(records[6]["status"], "protocol_error");
    assert_eq!(records[6]["error_code"], "-32602");
    assert_eq!(
        fs::metadata(&audit_log).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Receiver<Result<Value, String>>,
    reader: Option<JoinHandle<()>>,
    reaped: bool,
}

impl McpProcess {
    fn spawn(index_dir: &Path, socket: &Path, audit_log: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_awi"))
            .args(["--index-dir", path(index_dir), "--socket", path(socket)])
            .arg("mcp")
            .args(["--audit-log", path(audit_log)])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, responses) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let decoded = line.map_err(|error| error.to_string()).and_then(|line| {
                    serde_json::from_str(&line).map_err(|error| error.to_string())
                });
                if sender.send(decoded).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            responses,
            reader: Some(reader),
            reaped: false,
        }
    }

    fn request(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));
        let deadline = Instant::now() + RESPONSE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let response = self
                .responses
                .recv_timeout(remaining)
                .unwrap_or_else(|error| panic!("timed out waiting for MCP response {id}: {error}"))
                .unwrap_or_else(|error| panic!("invalid MCP response: {error}"));
            if response["id"] == id {
                return response;
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }));
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        serde_json::to_writer(&mut *stdin, &message).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn close(mut self) {
        drop(self.stdin.take());
        let deadline = Instant::now() + STOP_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "AWI MCP exited with {status}");
                self.reaped = true;
                break;
            }
            assert!(
                Instant::now() < deadline,
                "AWI MCP did not stop after stdin closed"
            );
            thread::sleep(Duration::from_millis(20));
        }
        if let Some(reader) = self.reader.take() {
            reader.join().unwrap();
        }
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        drop(self.stdin.take());
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn path(value: &Path) -> &str {
    value.to_str().unwrap()
}
