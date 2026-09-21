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
