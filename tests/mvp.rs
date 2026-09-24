use std::fs;
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use awi::{
    AgentDocumentRole, AgentMemoryLayer, AgentMemorySource, FileKind, IndexOptions, WorkspaceIndex,
};
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
        hit.path.ends_with("lib.rs") && hit.matched_lanes.iter().any(|lane| lane == "exact_symbol")
    }));
    let symbol_hit = symbol_hits
        .iter()
        .find(|hit| hit.path.ends_with("lib.rs"))
        .unwrap();
    assert_eq!(
        symbol_hit
            .symbol
            .as_ref()
            .map(|symbol| symbol.name.as_str()),
        Some("build_workspace_index")
    );
    assert!(
        symbol_hit
            .preview
            .starts_with("function build_workspace_index at lines 1-1")
    );

    let text_hits = workspace.search("total_spend", 10).unwrap();
    assert!(text_hits.iter().any(|hit| hit.path.ends_with("report.sql")));
    let cached_text_hits = workspace.search("total_spend", 10).unwrap();
    assert_eq!(cached_text_hits[0].path, text_hits[0].path);
    assert_eq!(workspace.status().unwrap().retrieval.cache_hits, 1);
    let sql_hits = workspace.search("SQL total_spend", 10).unwrap();
    assert!(sql_hits.iter().any(|hit| {
        hit.path.ends_with("report.sql") && hit.matched_lanes.iter().any(|lane| lane == "sql")
    }));

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
    let focused = workspace
        .inspect_symbol(&source, "build_workspace_index", 10, 1_000)
        .unwrap();
    assert_eq!(
        focused
            .focused_symbol
            .as_ref()
            .map(|symbol| symbol.name.as_str()),
        Some("build_workspace_index")
    );
    assert_eq!(focused.content.as_ref().unwrap().start_line, 1);
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

#[test]
fn retries_files_written_by_a_failed_derived_index_generation() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    let index_dir = fixture.path().join("index");
    let model = fixture.path().join("model.gguf");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("lib.rs"), "pub fn version_one() {}\n").unwrap();
    fs::write(&model, b"not-a-real-model").unwrap();

    let first = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args([
            "--index-dir",
            path(&index_dir),
            "index",
            path(&root),
            "--json",
        ])
        .output()
        .unwrap();
    assert_success(&first);

    fs::write(root.join("lib.rs"), "pub fn version_two() {}\n").unwrap();
    let failed = Command::new(env!("CARGO_BIN_EXE_awi"))
        .env("AWI_SEMANTIC_MODEL", &model)
        .env("AWI_SEMANTIC_PYTHON", "/bin/false")
        .args([
            "--index-dir",
            path(&index_dir),
            "index",
            path(&root),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!failed.status.success());

    let retry = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args([
            "--index-dir",
            path(&index_dir),
            "index",
            path(&root),
            "--json",
        ])
        .output()
        .unwrap();
    assert_success(&retry);
    let report: Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(report["indexed"], 1);
    assert_eq!(report["unchanged"], 0);
    assert!(
        WorkspaceIndex::open(&index_dir)
            .unwrap()
            .search("version_two", 5)
            .unwrap()
            .iter()
            .any(|hit| hit.path.ends_with("lib.rs"))
    );
}

#[test]
fn search_preview_is_centered_on_a_late_content_match() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    let index_dir = fixture.path().join("index");
    fs::create_dir_all(&root).unwrap();

    let document = root.join("notes.txt");
    let mut source = String::from("prefix_only_marker\n");
    for offset in 0..400 {
        source.push_str(&format!("ordinary filler line {offset}\n"));
    }
    source.push_str("late_match_anchor is the relevant evidence\n");
    fs::write(&document, source).unwrap();

    let mut workspace = WorkspaceIndex::open(&index_dir).unwrap();
    workspace
        .index_root(&root, &IndexOptions::default())
        .unwrap();

    let hits = workspace.search("late_match_anchor", 5).unwrap();
    let hit = hits
        .iter()
        .find(|hit| hit.path.ends_with("notes.txt"))
        .unwrap();
    assert!(hit.preview.contains("late_match_anchor"));
    assert!(!hit.preview.contains("prefix_only_marker"));
}

#[test]
fn indexes_agent_knowledge_with_scope_metadata_and_deduplication() {
    let fixture = tempdir().unwrap();
    let parent = fixture.path().join("parent");
    let workspace_root = parent.join("project");
    let service = workspace_root.join("services/api");
    let unrelated = workspace_root.join("other");
    let skills_root = fixture.path().join("global-skills");
    let index_dir = fixture.path().join("index");
    fs::create_dir_all(&service).unwrap();
    fs::create_dir_all(&unrelated).unwrap();
    fs::create_dir_all(skills_root.join("one/references")).unwrap();
    fs::create_dir_all(skills_root.join("duplicate")).unwrap();
    fs::create_dir_all(skills_root.join("secret")).unwrap();
    fs::create_dir_all(skills_root.join("malformed")).unwrap();

    let parent_agents = parent.join("AGENTS.md");
    let nested_agents = workspace_root.join("services/AGENTS.md");
    let unrelated_agents = unrelated.join("AGENTS.md");
    let context_file = service.join("main.rs");
    fs::write(
        &parent_agents,
        "# Parent Policy\nUse reviewed evidence for deployment.\n",
    )
    .unwrap();
    fs::write(
        &nested_agents,
        "# Service Policy\nUse scoped evidence for deployment.\n",
    )
    .unwrap();
    fs::write(
        &unrelated_agents,
        "# Other Policy\nUse unrelated evidence for deployment.\n",
    )
    .unwrap();
    fs::write(&context_file, "fn main() {}\n").unwrap();

    let skill = r#"---
name: evidence-check
description: Validate release evidence before deployment.
---

# Evidence Check

Read [acceptance](references/acceptance.md).
"#;
    fs::write(
        skills_root.join("one/references/acceptance.md"),
        "# Acceptance\n",
    )
    .unwrap();
    fs::write(skills_root.join("one/SKILL.md"), skill).unwrap();
    fs::write(skills_root.join("duplicate/SKILL.md"), skill).unwrap();
    let secret_skill = skills_root.join("secret/SKILL.md");
    fs::write(
        &secret_skill,
        "---\nname: leaked\n---\ntoken = ghp_abcdefghijklmnopqrstuvwxyz1234567890\n",
    )
    .unwrap();
    let malformed_skill = skills_root.join("malformed/SKILL.md");
    fs::write(
        &malformed_skill,
        "---\nname: [unterminated\n---\n# Fresh Malformed Metadata\n",
    )
    .unwrap();

    let mut workspace = WorkspaceIndex::open(&index_dir).unwrap();
    workspace
        .index_root(&workspace_root, &IndexOptions::default())
        .unwrap();
    workspace
        .index_root(&parent_agents, &IndexOptions::default())
        .unwrap();
    let skill_report = workspace
        .index_root(&skills_root, &IndexOptions::default())
        .unwrap();
    assert_eq!(skill_report.failed, 1);

    let status = workspace.status().unwrap();
    assert_eq!(status.agent_documents, 5);

    let scoped = workspace
        .search_filtered(
            "evidence deployment",
            10,
            &[],
            &["agent_instructions".to_owned()],
            None,
            Some(&context_file),
        )
        .unwrap();
    assert_eq!(scoped.len(), 2);
    assert!(scoped[0].path.ends_with("services/AGENTS.md"));
    assert!(scoped.iter().any(|hit| hit.path == parent_agents));
    assert!(
        scoped
            .iter()
            .all(|hit| !hit.path.ends_with("other/AGENTS.md"))
    );
    assert!(
        scoped
            .iter()
            .all(|hit| hit.matched_lanes.iter().any(|lane| lane == "agent"))
    );

    let skills = workspace
        .search_filtered(
            "release evidence",
            10,
            &[],
            &["agent_skill".to_owned()],
            None,
            Some(&context_file),
        )
        .unwrap();
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].kind, FileKind::AgentSkill);
    assert!(
        workspace
            .search("release evidence", 10)
            .unwrap()
            .iter()
            .all(|hit| hit.kind != FileKind::AgentSkill)
    );

    let inspected = workspace.inspect(skills_root.join("one/SKILL.md")).unwrap();
    let metadata = inspected.agent.unwrap();
    assert_eq!(metadata.role, AgentDocumentRole::Skill);
    assert_eq!(metadata.name.as_deref(), Some("evidence-check"));
    assert_eq!(metadata.headings, vec!["Evidence Check"]);
    assert_eq!(metadata.references.len(), 1);
    let sensitive = workspace.inspect(&secret_skill).unwrap();
    assert_eq!(
        sensitive.file.extraction_status,
        "metadata_only_sensitive_content,not_source"
    );
    assert!(sensitive.content.is_none());
    assert!(sensitive.agent.is_none());
    let malformed = workspace.inspect(&malformed_skill).unwrap();
    assert_eq!(
        malformed.file.extraction_status,
        "indexed,not_source,agent_parse_failed"
    );
    assert!(malformed.content.is_some());
    assert!(malformed.agent.is_none());

    fs::remove_file(&parent_agents).unwrap();
    let deleted = workspace
        .index_root(&parent_agents, &IndexOptions::default())
        .unwrap();
    assert_eq!(deleted.deleted, 1);
    assert_eq!(workspace.status().unwrap().agent_documents, 4);
    assert!(
        workspace
            .search_filtered(
                "Parent Policy",
                10,
                &[],
                &["agent_instructions".to_owned()],
                None,
                Some(&context_file),
            )
            .unwrap()
            .iter()
            .all(|hit| hit.path != parent_agents)
    );
}

#[test]
fn deletes_registered_agent_root_after_parent_directory_disappears() {
    let fixture = tempdir().unwrap();
    let skill_dir = fixture.path().join("transient-skill");
    let skill = skill_dir.join("SKILL.md");
    let index_dir = fixture.path().join("index");
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        &skill,
        "---\nname: transient\ndescription: Temporary indexed knowledge.\n---\n",
    )
    .unwrap();

    let mut workspace = WorkspaceIndex::open(&index_dir).unwrap();
    workspace
        .index_root(&skill, &IndexOptions::default())
        .unwrap();
    fs::remove_dir_all(&skill_dir).unwrap();
    let deleted = workspace
        .index_root(&skill, &IndexOptions::default())
        .unwrap();
    assert_eq!(deleted.deleted, 1);
    assert_eq!(workspace.status().unwrap().agent_documents, 0);
}

#[test]
fn indexes_project_scoped_agent_memory_safely_and_incrementally() {
    let fixture = tempdir().unwrap();
    let project_a = fixture.path().join("project-a");
    let project_b = fixture.path().join("project-b");
    let memory_a = fixture.path().join("memory-a");
    let memory_b = fixture.path().join("memory-b");
    let duplicate = fixture.path().join("duplicate.md");
    let raw = fixture.path().join("raw-session.jsonl");
    let index_dir = fixture.path().join("index");
    fs::create_dir_all(project_a.join("src")).unwrap();
    fs::create_dir_all(project_b.join("src")).unwrap();
    fs::create_dir_all(&memory_a).unwrap();
    fs::create_dir_all(&memory_b).unwrap();
    fs::write(project_a.join("src/lib.rs"), "fn alpha() {}\n").unwrap();
    fs::write(project_b.join("src/lib.rs"), "fn beta() {}\n").unwrap();
    fs::write(
        memory_a.join("project_memory.md"),
        "# Project memory\nvalidated delta protocol\n",
    )
    .unwrap();
    fs::write(
        memory_a.join("MEMORY.md"),
        "# Aggregate memory\nDecision Gateway rollout status.\n",
    )
    .unwrap();
    fs::write(
        memory_a.join("decision_gateway.md"),
        "---\nname: Decision Gateway\ndescription: Choice, score, and retry routing decisions.\n---\n# Decision\nDecision Gateway rollout status.\n",
    )
    .unwrap();
    fs::write(
        memory_a.join("session_memory_session-a.jsonl"),
        r#"{"intent":"inspect append marker","actions":["first pass"],"outcome":"ready","message_id":"not-indexed"}"#,
    )
    .unwrap();
    fs::write(
        memory_a.join("secret.md"),
        "token = ghp_abcdefghijklmnopqrstuvwxyz1234567890\n",
    )
    .unwrap();
    fs::write(
        memory_b.join("project_memory.md"),
        "# Other memory\nforeign omega protocol\n",
    )
    .unwrap();
    fs::write(&duplicate, "# Project memory\nvalidated delta protocol\n").unwrap();
    fs::write(
        &raw,
        r#"{"role":"user","content":"validated delta protocol raw archive"}"#,
    )
    .unwrap();

    let source = |path, agent: &str, workspace_root, raw_history| AgentMemorySource {
        path,
        agent: agent.to_owned(),
        workspace_root: Some(workspace_root),
        project_key: None,
        raw_history,
    };
    let mut workspace = WorkspaceIndex::open(&index_dir).unwrap();
    workspace
        .index_root(&project_a, &IndexOptions::default())
        .unwrap();
    workspace
        .index_root(&project_b, &IndexOptions::default())
        .unwrap();
    for memory_source in [
        source(memory_a.clone(), "trae", project_a.clone(), false),
        source(memory_b.clone(), "trae", project_b.clone(), false),
        source(duplicate.clone(), "zcode", project_a.clone(), false),
        source(raw.clone(), "gemini", project_a.clone(), true),
    ] {
        workspace
            .index_agent_memory_source(&memory_source, &IndexOptions::default())
            .unwrap();
    }

    let initial_status = workspace.status().unwrap();
    assert_eq!(initial_status.agent_memories, 8);
    assert_eq!(initial_status.memory.registered_sources, 4);
    assert_eq!(initial_status.memory.active_files, 8);
    assert_eq!(initial_status.memory.stale_files, 0);
    assert_eq!(initial_status.memory.missing_files, 0);
    assert!(
        workspace
            .search("validated delta protocol", 10)
            .unwrap()
            .iter()
            .all(|hit| hit.kind != FileKind::AgentMemory)
    );
    let hits = workspace
        .search_filtered(
            "validated delta protocol",
            10,
            std::slice::from_ref(&project_a),
            &["agent_memory".to_owned()],
            None,
            Some(&project_a.join("src/lib.rs")),
        )
        .unwrap();
    assert_eq!(
        hits.iter()
            .filter(|hit| {
                hit.memory
                    .as_ref()
                    .is_some_and(|memory| memory.layer == AgentMemoryLayer::ProjectSummary)
            })
            .count(),
        1,
        "content-identical project summaries must collapse"
    );
    assert_eq!(
        hits[0].memory.as_ref().unwrap().layer,
        AgentMemoryLayer::ProjectSummary
    );
    assert!(
        hits.iter()
            .all(|hit| !Path::new(&hit.path).starts_with(memory_b.as_path()))
    );
    let ranked_memory = workspace
        .search_filtered(
            "decision_gateway rollout status",
            10,
            &[],
            &["agent_memory".to_owned()],
            None,
            Some(&project_a),
        )
        .unwrap();
    assert!(
        ranked_memory[0].path.ends_with("decision_gateway.md"),
        "frontmatter and exact filename/name matches should outrank aggregate memory: {:?}",
        ranked_memory
            .iter()
            .map(|hit| (&hit.path, hit.score))
            .collect::<Vec<_>>()
    );
    let ranked_metadata = ranked_memory[0].memory.as_ref().unwrap();
    assert_eq!(ranked_metadata.name.as_deref(), Some("Decision Gateway"));
    assert_eq!(
        ranked_metadata.description.as_deref(),
        Some("Choice, score, and retry routing decisions.")
    );

    let session = memory_a.join("session_memory_session-a.jsonl");
    let inspected = workspace.inspect(&session).unwrap();
    let metadata = inspected.memory.unwrap();
    assert_eq!(metadata.agent, "trae");
    assert_eq!(metadata.layer, AgentMemoryLayer::SessionSummary);
    assert_eq!(metadata.session_id.as_deref(), Some("session-a"));
    assert!(inspected.dataset.is_none());

    let sensitive = workspace.inspect(memory_a.join("secret.md")).unwrap();
    assert_eq!(
        sensitive.file.extraction_status,
        "metadata_only_sensitive_content,not_source,memory_parsed"
    );
    assert!(sensitive.content.is_none());

    thread::sleep(Duration::from_millis(2));
    fs::write(
        &session,
        r#"{"intent":"inspect append marker","actions":["second pass"],"outcome":"fresh appendix token"}"#,
    )
    .unwrap();
    let stale_status = workspace.status().unwrap();
    assert_eq!(stale_status.memory.stale_files, 1);
    assert!(stale_status.memory.max_source_lag_ms >= 1);
    workspace
        .index_agent_memory_source(
            &source(memory_a, "trae", project_a.clone(), false),
            &IndexOptions::default(),
        )
        .unwrap();
    assert_eq!(workspace.status().unwrap().memory.stale_files, 0);
    let refreshed = workspace
        .search_filtered(
            "fresh appendix token",
            10,
            &[],
            &["agent_memory".to_owned()],
            None,
            Some(&project_a),
        )
        .unwrap();
    assert_eq!(refreshed[0].path, session);
    assert!(refreshed[0].preview.contains("fresh appendix token"));
    assert!(!refreshed[0].preview.contains("message_id"));

    fs::remove_file(&session).unwrap();
    assert_eq!(workspace.status().unwrap().memory.missing_files, 1);
    workspace
        .index_agent_memory_source(
            &source(
                fixture.path().join("memory-a"),
                "trae",
                project_a.clone(),
                false,
            ),
            &IndexOptions::default(),
        )
        .unwrap();
    assert_eq!(workspace.status().unwrap().memory.missing_files, 0);
    assert!(
        workspace
            .search_filtered(
                "fresh appendix token",
                10,
                &[],
                &["agent_memory".to_owned()],
                None,
                Some(&project_a),
            )
            .unwrap()
            .is_empty()
    );
}

const START_TIMEOUT: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn prune_generation_history_bounds_repeated_reconciles() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    let index_dir = fixture.path().join("index");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("lib.rs"), "pub fn f() -> usize { 1 }\n").unwrap();

    let mut workspace = WorkspaceIndex::open(&index_dir).unwrap();
    // Reconcile many times with no content change: each appends a generation
    // bookkeeping row even though nothing is published.
    for _ in 0..12 {
        workspace
            .index_root(&root, &IndexOptions::default())
            .unwrap();
    }
    let latest = workspace.status().unwrap().completed_generation.unwrap();
    // Pruning keeps rows at or above the latest completed generation and drops
    // the rest, so the table cannot grow without bound.
    let removed = workspace.prune_generation_history().unwrap();
    assert!(
        removed >= 11,
        "expected to prune stale rows, removed {removed}"
    );
    // The latest completed generation is unchanged and still searchable.
    assert_eq!(
        workspace.status().unwrap().completed_generation,
        Some(latest)
    );
    assert!(
        workspace
            .search("f", 5)
            .unwrap()
            .iter()
            .any(|hit| hit.path.ends_with("lib.rs"))
    );
    // Pruning again is a no-op now that only current rows remain.
    assert_eq!(workspace.prune_generation_history().unwrap(), 0);
}

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

    let mut disconnected = UnixStream::connect(&socket).unwrap();
    disconnected
        .write_all(b"{\"protocol_version\":3,\"request\":{\"method\":\"ping\"}}\n")
        .unwrap();
    disconnected.shutdown(Shutdown::Both).unwrap();
    drop(disconnected);
    thread::sleep(Duration::from_millis(20));
    let ping_after_disconnect = run_json(&index_dir, &socket, &["ping", "--json"]);
    assert_eq!(ping_after_disconnect["service"], "awi");

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
    assert_eq!(status["serving"]["workers"], 1);
    assert!(status["retrieval"]["searches"].as_u64().unwrap() >= 1);

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

#[test]
fn snapshot_daemon_serves_around_a_stalled_connection() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    let writer_index = fixture.path().join("writer-index");
    let publish_dir = fixture.path().join("published");
    let runtime_index = fixture.path().join("runtime-index");
    let socket = fixture.path().join("snapshot.sock");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("service.rs"),
        "pub fn concurrent_snapshot_target() -> usize { 1 }\n",
    )
    .unwrap();
    {
        let mut workspace = WorkspaceIndex::open(&writer_index).unwrap();
        workspace
            .index_root(&root, &IndexOptions::default())
            .unwrap();
        workspace.publish_snapshot(&publish_dir).unwrap();
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
            "1000",
        ])
        .env("AWI_DAEMON_QUERY_WORKERS", "2")
        .env("AWI_DAEMON_QUERY_QUEUE_CAPACITY", "2")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut daemon = ChildGuard::new(child);
    wait_until_ready(&mut daemon, &runtime_index, &socket);

    let blocked = UnixStream::connect(&socket).unwrap();
    thread::sleep(Duration::from_millis(50));
    let request_index = runtime_index.clone();
    let request_socket = socket.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    let request = thread::spawn(move || {
        let value = run_json(&request_index, &request_socket, &["status", "--json"]);
        sender.send(value).unwrap();
    });
    let status = receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("second snapshot worker did not serve around a stalled connection");
    assert_eq!(status["serving"]["workers"], 2);
    assert!(status["serving"]["active"].as_u64().unwrap() >= 2);
    drop(blocked);
    request.join().unwrap();

    run_json(&runtime_index, &socket, &["stop", "--json"]);
    wait_until_stopped(&mut daemon);
}

#[test]
fn watch_producer_auto_publishes_and_reader_follows() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    let writer_index = fixture.path().join("writer-index");
    let publish_dir = fixture.path().join("published");
    let runtime_index = fixture.path().join("runtime-index");
    let socket = fixture.path().join("watch.sock");
    let source = root.join("service.rs");
    fs::create_dir_all(&root).unwrap();
    fs::write(&source, "pub fn watch_version_one() -> usize { 1 }\n").unwrap();

    // Seed one indexed root so `watch` can default to catalog roots.
    {
        let mut workspace = WorkspaceIndex::open(&writer_index).unwrap();
        workspace
            .index_root(&root, &IndexOptions::default())
            .unwrap();
    }

    // Start the persistent producer: reconcile + auto-publish on change.
    let producer = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args(["--index-dir", path(&writer_index)])
        .arg("watch")
        .args([
            "--publish-dir",
            path(&publish_dir),
            "--interval-ms",
            "50",
            "--retain",
            "1",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut producer = ChildGuard::new(producer);

    wait_for_pointer(&publish_dir, &mut producer);

    // Start the snapshot-following reader and confirm it sees the first version.
    let reader = Command::new(env!("CARGO_BIN_EXE_awi"))
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
    let mut reader = ChildGuard::new(reader);
    wait_until_ready(&mut reader, &runtime_index, &socket);
    wait_for_hits(&runtime_index, &socket, "watch_version_one", &mut reader);

    // Change the source; the producer republishes and the reader auto-switches.
    fs::write(&source, "pub fn watch_version_two() -> usize { 2 }\n").unwrap();
    wait_for_hits(&runtime_index, &socket, "watch_version_two", &mut reader);
    let stale = run_json(
        &runtime_index,
        &socket,
        &["search", "watch_version_one", "--json"],
    );
    assert!(stale.as_array().unwrap().is_empty());

    run_json(&runtime_index, &socket, &["stop", "--json"]);
    wait_until_stopped(&mut reader);
    producer.child.kill().unwrap();
    producer.child.wait().unwrap();
    producer.reaped = true;
}

#[test]
fn watch_producer_rediscovers_registered_memory_projects() {
    let fixture = tempdir().unwrap();
    let home = fixture.path().join("home");
    let root = fixture.path().join("workspace");
    let writer_index = fixture.path().join("writer-index");
    let publish_dir = fixture.path().join("published");
    fs::create_dir_all(home.join(".trae-cn/memory")).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("lib.rs"), "pub fn watched_workspace() {}\n").unwrap();
    fs::write(
        home.join(".trae-cn/memory/user_profile.md"),
        "# Profile\nKeep memory current.\n",
    )
    .unwrap();

    let registered = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args(["--index-dir", path(&writer_index)])
        .arg("memory")
        .args(["--project-root", path(&root), "--json"])
        .env("HOME", &home)
        .output()
        .unwrap();
    assert_success(&registered);

    let producer = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args(["--index-dir", path(&writer_index)])
        .arg("watch")
        .args([
            "--root",
            path(&root),
            "--publish-dir",
            path(&publish_dir),
            "--interval-ms",
            "50",
            "--retain",
            "1",
        ])
        .env("HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut producer = ChildGuard::new(producer);
    wait_for_pointer(&publish_dir, &mut producer);

    let encoded_root = root.to_string_lossy().replace('/', "-");
    let discovered = home
        .join(".trae-cn/memory/projects")
        .join(format!("{encoded_root}--p2-watch"));
    fs::create_dir_all(&discovered).unwrap();
    let memory_file = discovered.join("periodic_discovery.md");
    fs::write(
        &memory_file,
        "---\nname: Periodic Discovery\n---\nperiodic rediscovery sentinel\n",
    )
    .unwrap();

    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(status) = producer.child.try_wait().unwrap() {
            panic!("AWI producer exited before rediscovering memory: {status}");
        }
        let output = Command::new(env!("CARGO_BIN_EXE_awi"))
            .args(["--index-dir", path(&writer_index)])
            .arg("search")
            .arg("periodic rediscovery sentinel")
            .args(["--kind", "agent_memory", "--context-path", path(&root)])
            .arg("--json")
            .env("HOME", &home)
            .output()
            .unwrap();
        if output.status.success()
            && serde_json::from_slice::<Value>(&output.stdout)
                .ok()
                .and_then(|value| value.as_array().cloned())
                .is_some_and(|hits| {
                    hits.iter().any(|hit| {
                        hit["path"]
                            .as_str()
                            .is_some_and(|path| Path::new(path) == memory_file.as_path())
                    })
                })
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watch producer did not rediscover the new memory source"
        );
        thread::sleep(Duration::from_millis(20));
    }

    let status = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args(["--index-dir", path(&writer_index), "status", "--json"])
        .env("HOME", &home)
        .output()
        .unwrap();
    assert_success(&status);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["memory"]["registered_projects"], 1);
    assert_eq!(status["memory"]["registered_sources"], 2);
    assert_eq!(status["memory"]["active_files"], 2);
    assert_eq!(status["memory"]["stale_files"], 0);
    assert_eq!(status["memory"]["missing_files"], 0);

    fs::remove_dir_all(&discovered).unwrap();
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        let output = Command::new(env!("CARGO_BIN_EXE_awi"))
            .args(["--index-dir", path(&writer_index)])
            .arg("search")
            .arg("periodic rediscovery sentinel")
            .args(["--kind", "agent_memory", "--context-path", path(&root)])
            .arg("--json")
            .env("HOME", &home)
            .output()
            .unwrap();
        if output.status.success()
            && serde_json::from_slice::<Value>(&output.stdout)
                .ok()
                .and_then(|value| value.as_array().cloned())
                .is_some_and(|hits| hits.is_empty())
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "watch producer did not remove the missing memory source"
        );
        thread::sleep(Duration::from_millis(20));
    }

    producer.child.kill().unwrap();
    producer.child.wait().unwrap();
    producer.reaped = true;
}

#[test]
fn watch_producer_reacts_to_local_edits_before_periodic_tick() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    let writer_index = fixture.path().join("writer-index");
    let publish_dir = fixture.path().join("published");
    let source = root.join("service.rs");
    fs::create_dir_all(&root).unwrap();
    fs::write(&source, "pub fn realtime_version_one() -> usize { 1 }\n").unwrap();

    {
        let mut workspace = WorkspaceIndex::open(&writer_index).unwrap();
        workspace
            .index_root(&root, &IndexOptions::default())
            .unwrap();
    }

    // A very long interval means any timely republish must come from the
    // real-time filesystem watcher, not the periodic safety-net tick.
    let producer = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args(["--index-dir", path(&writer_index)])
        .arg("watch")
        .args([
            "--publish-dir",
            path(&publish_dir),
            "--interval-ms",
            "600000",
            "--debounce-ms",
            "50",
            "--retain",
            "2",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut producer = ChildGuard::new(producer);
    wait_for_pointer(&publish_dir, &mut producer);
    let first = read_pointer_generation(&publish_dir);

    // Edit a local-disk file; the watcher should drive a new publish quickly,
    // well within the START_TIMEOUT and far below the 600s interval.
    fs::write(&source, "pub fn realtime_version_two() -> usize { 2 }\n").unwrap();
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(status) = producer.child.try_wait().unwrap() {
            panic!("AWI producer exited before reacting: {status}");
        }
        if read_pointer_generation(&publish_dir) > first {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "real-time watcher did not republish within {START_TIMEOUT:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }

    producer.child.kill().unwrap();
    producer.child.wait().unwrap();
    producer.reaped = true;
}

fn read_pointer_generation(publish_dir: &Path) -> i64 {
    let pointer = publish_dir.join("current.json");
    let value: Value = serde_json::from_slice(&fs::read(&pointer).unwrap()).unwrap();
    value["generation"].as_i64().unwrap()
}

fn wait_for_pointer(publish_dir: &Path, producer: &mut ChildGuard) {
    let deadline = Instant::now() + START_TIMEOUT;
    let pointer = publish_dir.join("current.json");
    loop {
        if let Some(status) = producer.child.try_wait().unwrap() {
            panic!("AWI producer exited before publishing: {status}");
        }
        if pointer.exists() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "producer did not publish a snapshot within {START_TIMEOUT:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn wait_for_hits(index_dir: &Path, socket: &Path, query: &str, reader: &mut ChildGuard) {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Some(status) = reader.child.try_wait().unwrap() {
            panic!("AWI reader exited before serving {query}: {status}");
        }
        let hits = run_json(index_dir, socket, &["search", query, "--json"]);
        if hits.as_array().is_some_and(|hits| !hits.is_empty()) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "reader did not observe {query} within {START_TIMEOUT:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }
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
