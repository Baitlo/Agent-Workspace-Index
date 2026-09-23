use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use tempfile::tempdir;

const NEW_CLIENTS: &str = "qwen,cline,zed,amazon-q,crush";

#[test]
fn configures_new_clients_privately_and_idempotently() {
    let fixture = tempdir().unwrap();
    let home = fixture.path().join("home");
    let config_home = fixture.path().join("config");
    let bin = fixture.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    for executable in ["qwen", "cline", "zed", "q", "crush"] {
        symlink("/bin/true", bin.join(executable)).unwrap();
    }

    let first = run_integrate(fixture.path(), &home, &config_home, &bin);
    assert!(
        first.status.success(),
        "first integration failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let first_report: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_statuses(&first_report, "configured");

    let paths = [
        home.join(".qwen/settings.json"),
        home.join(".cline/data/settings/cline_mcp_settings.json"),
        config_home.join("zed/settings.json"),
        home.join(".aws/amazonq/mcp.json"),
        config_home.join("crush/crush.json"),
    ];
    for path in &paths {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600,
            "{} is not private",
            path.display()
        );
    }

    assert_eq!(read_json(&paths[0])["mcpServers"]["awi"]["timeout"], 45_000);
    assert_eq!(
        read_json(&paths[1])["mcpServers"]["awi"]["transport"]["type"],
        "stdio"
    );
    assert_eq!(
        read_json(&paths[2])["context_servers"]["awi"]["command"],
        "/bin/echo"
    );
    assert_eq!(read_json(&paths[3])["mcpServers"]["awi"]["timeout"], 45_000);
    assert_eq!(read_json(&paths[4])["mcp"]["awi"]["timeout"], 45);

    let second = run_integrate(fixture.path(), &home, &config_home, &bin);
    assert!(
        second.status.success(),
        "second integration failed: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second_report: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_statuses(&second_report, "already_configured");
}

#[test]
fn configures_clients_with_existing_jsonc_files() {
    let fixture = tempdir().unwrap();
    let home = fixture.path().join("home");
    let config_home = fixture.path().join("config");
    let bin = fixture.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::create_dir_all(config_home.join("opencode")).unwrap();
    fs::create_dir_all(config_home.join("zed")).unwrap();
    symlink("/bin/true", bin.join("opencode")).unwrap();
    symlink("/bin/true", bin.join("zed")).unwrap();
    fs::write(
        config_home.join("opencode/opencode.jsonc"),
        "{\n  // keep the setting\n  \"theme\": \"dark\",\n}\n",
    )
    .unwrap();
    fs::write(
        config_home.join("zed/settings.json"),
        "{\n  // keep the setting\n  \"theme\": \"One Dark\",\n}\n",
    )
    .unwrap();

    let output = run_integrate_clients(fixture.path(), &home, &config_home, &bin, "opencode,zed");
    assert!(
        output.status.success(),
        "integration failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_statuses_count(&report, "configured", 2);
    assert_eq!(
        read_json(&config_home.join("opencode/opencode.jsonc"))["theme"],
        "dark"
    );
    assert_eq!(
        read_json(&config_home.join("zed/settings.json"))["theme"],
        "One Dark"
    );
}

#[test]
fn installer_indexes_ancestor_instructions_and_allowlisted_skill_manifests() {
    let fixture = tempdir().unwrap();
    let home = fixture.path().join("home");
    let workspace = home.join("projects/repository");
    let context_file = workspace.join("src/lib.rs");
    let skill = home.join(".agents/skills/release-check/SKILL.md");
    let parent_skill = home.join("projects/.trae/skills/team-policy/SKILL.md");
    let linked_skill = fixture.path().join("shared/linked-skill/SKILL.md");
    let encoded_workspace = workspace.to_string_lossy().replace('/', "-");
    let trae_memory = home
        .join(".trae-cn/memory/projects")
        .join(format!("{encoded_workspace}--p2-test"));
    let gemini_raw = home.join(".gemini/tmp/repository/chats");
    let index = fixture.path().join("index");
    let bin = fixture.path().join("installed");
    fs::create_dir_all(context_file.parent().unwrap()).unwrap();
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::create_dir_all(parent_skill.parent().unwrap()).unwrap();
    fs::create_dir_all(linked_skill.parent().unwrap()).unwrap();
    fs::create_dir_all(&trae_memory).unwrap();
    fs::create_dir_all(home.join(".trae-cn/memory")).unwrap();
    fs::create_dir_all(&gemini_raw).unwrap();
    let archived_skill = home.join(".agents/skills/.archive/old/SKILL.md");
    fs::create_dir_all(archived_skill.parent().unwrap()).unwrap();
    fs::write(
        home.join("projects/AGENTS.md"),
        "# Project Rules\nVerify release evidence.\n",
    )
    .unwrap();
    fs::write(&context_file, "pub fn release() {}\n").unwrap();
    fs::write(
        &skill,
        "---\nname: release-check\ndescription: Validate release evidence.\n---\n# Workflow\n",
    )
    .unwrap();
    fs::write(
        parent_skill,
        "---\nname: team-policy\ndescription: Apply parent workspace policy.\n---\n",
    )
    .unwrap();
    fs::write(
        &linked_skill,
        "---\nname: linked-skill\ndescription: Follow symlinked Skill manifests.\n---\n",
    )
    .unwrap();
    symlink(
        linked_skill.parent().unwrap(),
        home.join(".agents/skills/linked"),
    )
    .unwrap();
    fs::write(
        archived_skill,
        "---\nname: archived\ndescription: Must not be indexed.\n---\n",
    )
    .unwrap();
    fs::write(
        home.join(".trae-cn/memory/user_profile.md"),
        "# Profile\nPrefer concise evidence.\n",
    )
    .unwrap();
    fs::write(
        trae_memory.join("project_memory.md"),
        "# Memory\nRemember durable release checksum.\n",
    )
    .unwrap();
    fs::write(
        trae_memory.join("session_memory_session-1.jsonl"),
        r#"{"intent":"release memory","outcome":"checksum verified"}"#,
    )
    .unwrap();
    fs::write(
        home.join(".gemini/projects.json"),
        format!(
            r#"{{"projects":{{"{}":"repository"}}}}"#,
            workspace.display()
        ),
    )
    .unwrap();
    fs::write(
        gemini_raw.join("session-raw.jsonl"),
        r#"{"role":"user","content":"must remain opt in"}"#,
    )
    .unwrap();

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh");
    let output = Command::new("bash")
        .arg(script)
        .args(["--workspace", workspace.to_str().unwrap()])
        .args(["--index-dir", index.to_str().unwrap()])
        .args(["--bin-dir", bin.to_str().unwrap()])
        .args(["--source-binary", env!("CARGO_BIN_EXE_awi")])
        .args(["--clients", "qwen"])
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", fixture.path().join("config"))
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "installer failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Indexing 4 Agent knowledge root(s)"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Discovering Agent memory"));

    let status = Command::new(bin.join("awi"))
        .args(["--index-dir", index.to_str().unwrap(), "status", "--json"])
        .output()
        .unwrap();
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["agent_documents"], 4);
    assert_eq!(status["agent_memories"], 3);

    let instructions = Command::new(bin.join("awi"))
        .args(["--index-dir", index.to_str().unwrap(), "search"])
        .arg("release evidence")
        .args(["--kind", "agent_instructions", "--context-path"])
        .arg(&context_file)
        .arg("--json")
        .output()
        .unwrap();
    assert!(instructions.status.success());
    let instructions: Value = serde_json::from_slice(&instructions.stdout).unwrap();
    assert_eq!(instructions[0]["agent"]["role"], "instructions");

    let skills = Command::new(bin.join("awi"))
        .args(["--index-dir", index.to_str().unwrap(), "search"])
        .arg("Validate release evidence")
        .args(["--kind", "agent_skill", "--json"])
        .output()
        .unwrap();
    assert!(skills.status.success());
    let skills: Value = serde_json::from_slice(&skills.stdout).unwrap();
    assert_eq!(skills[0]["agent"]["name"], "release-check");

    let memories = Command::new(bin.join("awi"))
        .args(["--index-dir", index.to_str().unwrap(), "search"])
        .arg("release checksum")
        .args(["--kind", "agent_memory", "--context-path"])
        .arg(&context_file)
        .arg("--json")
        .output()
        .unwrap();
    assert!(memories.status.success());
    let memories: Value = serde_json::from_slice(&memories.stdout).unwrap();
    assert_eq!(memories[0]["memory"]["agent"], "trae");

    let raw = Command::new(bin.join("awi"))
        .args(["--index-dir", index.to_str().unwrap(), "search"])
        .arg("must remain opt in")
        .args(["--kind", "agent_memory", "--json"])
        .output()
        .unwrap();
    assert!(raw.status.success());
    let raw: Value = serde_json::from_slice(&raw.stdout).unwrap();
    assert_eq!(raw.as_array().unwrap().len(), 0);
}

#[test]
fn installer_falls_back_to_a_user_writable_pi_adapter_install() {
    let fixture = tempdir().unwrap();
    let home = fixture.path().join("home");
    let workspace = fixture.path().join("workspace");
    let index = fixture.path().join("index");
    let installed_bin = fixture.path().join("installed");
    let test_bin = fixture.path().join("test-bin");
    let pi_home = home.join(".pi/agent");
    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&test_bin).unwrap();
    fs::write(workspace.join("readme.txt"), "Pi fallback fixture.\n").unwrap();

    let pi = test_bin.join("pi");
    fs::write(
        &pi,
        r#"#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  list)
    [[ -f "$PI_CODING_AGENT_DIR/adapter-installed" ]] &&
      printf 'npm/node_modules/pi-mcp-adapter\n'
    ;;
  install)
    [[ "${2:-}" != npm:* ]] || exit 1
    [[ -d "${2:-}" ]]
    mkdir -p "$PI_CODING_AGENT_DIR"
    touch "$PI_CODING_AGENT_DIR/adapter-installed"
    ;;
  *) exit 2 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&pi, fs::Permissions::from_mode(0o755)).unwrap();

    let npm = test_bin.join("npm");
    fs::write(
        &npm,
        r#"#!/usr/bin/env bash
set -euo pipefail
prefix=
while (($# > 0)); do
  if [[ "$1" == "--prefix" ]]; then
    prefix="$2"
    shift 2
  else
    shift
  fi
done
mkdir -p "$prefix/node_modules/pi-mcp-adapter"
"#,
    )
    .unwrap();
    fs::set_permissions(&npm, fs::Permissions::from_mode(0o755)).unwrap();

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/install.sh");
    let output = Command::new("bash")
        .arg(script)
        .args(["--workspace", workspace.to_str().unwrap()])
        .args(["--index-dir", index.to_str().unwrap()])
        .args(["--bin-dir", installed_bin.to_str().unwrap()])
        .args(["--source-binary", env!("CARGO_BIN_EXE_awi")])
        .args(["--clients", "pi"])
        .args(["--skip-agent-knowledge", "--skip-agent-memory"])
        .env("HOME", &home)
        .env("PI_CODING_AGENT_DIR", &pi_home)
        .env("PATH", format!("{}:/usr/bin:/bin", test_bin.display()))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "installer failed: stdout={}, stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(pi_home.join("adapter-installed").is_file());
    assert!(pi_home.join("npm/node_modules/pi-mcp-adapter").is_dir());
    assert_eq!(
        read_json(&pi_home.join("mcp.json"))["mcpServers"]["awi"]["transport"],
        "stdio"
    );
}

#[test]
fn semantic_build_exposes_resume_mode() {
    let output = Command::new(env!("CARGO_BIN_EXE_awi"))
        .args(["semantic-build", "--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("--resume"),
        "semantic-build help did not expose --resume: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn run_integrate(
    fixture: &Path,
    home: &Path,
    config_home: &Path,
    bin: &Path,
) -> std::process::Output {
    run_integrate_clients(fixture, home, config_home, bin, NEW_CLIENTS)
}

fn run_integrate_clients(
    fixture: &Path,
    home: &Path,
    config_home: &Path,
    bin: &Path,
    clients: &str,
) -> std::process::Output {
    let path = format!("{}:/usr/bin:/bin", bin.display());
    Command::new(env!("CARGO_BIN_EXE_awi"))
        .args([
            "--index-dir",
            fixture.join("index").to_str().unwrap(),
            "integrate",
            "--client",
            clients,
            "--server-command",
            "/bin/echo",
            "--server-arg",
            "mcp",
            "--project-root",
            fixture.to_str().unwrap(),
            "--json",
        ])
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", config_home)
        .env("PATH", path)
        .output()
        .unwrap()
}

fn assert_statuses(report: &Value, expected: &str) {
    assert_statuses_count(report, expected, 5);
}

fn assert_statuses_count(report: &Value, expected: &str, count: usize) {
    let clients = report["clients"].as_array().unwrap();
    assert_eq!(clients.len(), count);
    assert!(
        clients.iter().all(|client| client["status"] == expected),
        "unexpected report: {report}"
    );
}

fn read_json(path: &PathBuf) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
