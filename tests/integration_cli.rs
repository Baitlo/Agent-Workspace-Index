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
        home.join(".cline/mcp.json"),
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
    assert_eq!(read_json(&paths[1])["mcpServers"]["awi"]["disabled"], false);
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
fn installer_indexes_ancestor_instructions_and_allowlisted_skill_manifests() {
    let fixture = tempdir().unwrap();
    let home = fixture.path().join("home");
    let workspace = home.join("projects/repository");
    let context_file = workspace.join("src/lib.rs");
    let skill = home.join(".agents/skills/release-check/SKILL.md");
    let parent_skill = home.join("projects/.trae/skills/team-policy/SKILL.md");
    let linked_skill = fixture.path().join("shared/linked-skill/SKILL.md");
    let index = fixture.path().join("index");
    let bin = fixture.path().join("installed");
    fs::create_dir_all(context_file.parent().unwrap()).unwrap();
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::create_dir_all(parent_skill.parent().unwrap()).unwrap();
    fs::create_dir_all(linked_skill.parent().unwrap()).unwrap();
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

    let status = Command::new(bin.join("awi"))
        .args(["--index-dir", index.to_str().unwrap(), "status", "--json"])
        .output()
        .unwrap();
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["agent_documents"], 4);

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
}

fn run_integrate(
    fixture: &Path,
    home: &Path,
    config_home: &Path,
    bin: &Path,
) -> std::process::Output {
    let path = format!("{}:/usr/bin:/bin", bin.display());
    Command::new(env!("CARGO_BIN_EXE_awi"))
        .args([
            "--index-dir",
            fixture.join("index").to_str().unwrap(),
            "integrate",
            "--client",
            NEW_CLIENTS,
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
    let clients = report["clients"].as_array().unwrap();
    assert_eq!(clients.len(), 5);
    assert!(
        clients.iter().all(|client| client["status"] == expected),
        "unexpected report: {report}"
    );
}

fn read_json(path: &PathBuf) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
