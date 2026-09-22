#!/usr/bin/env python3
"""Exercise AWI integration against installed coding-agent harnesses."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import stat
import subprocess
import time
from pathlib import Path
from typing import Any

CLIENTS = [
    "codex",
    "gemini",
    "claude",
    "copilot",
    "trae",
    "zcode",
    "kimi",
    "opencode",
    "pi",
    "cursor",
    "windsurf",
    "qwen",
    "cline",
    "zed",
    "amazon-q",
    "crush",
]

CONFIG_PATHS = {
    "codex": ".codex/config.toml",
    "gemini": ".gemini/settings.json",
    "claude": ".claude.json",
    "copilot": ".copilot/mcp-config.json",
    "zcode": ".zcode/cli/config.json",
    "kimi": ".kimi-code/mcp.json",
    "pi": ".pi/agent/mcp.json",
    "cursor": ".cursor/mcp.json",
    "windsurf": ".codeium/windsurf/mcp_config.json",
    "qwen": ".qwen/settings.json",
    "cline": ".cline/data/settings/cline_mcp_settings.json",
    "amazon-q": ".aws/amazonq/mcp.json",
}

XDG_CONFIG_PATHS = {
    "opencode": "opencode/opencode.json",
    "zed": "zed/settings.json",
    "crush": "crush/crush.json",
}

PROBES = {
    "codex": ["codex", "mcp", "get", "awi", "--json"],
    "gemini": ["gemini", "mcp", "list"],
    "claude": ["claude", "mcp", "get", "awi"],
    "copilot": ["copilot", "mcp", "get", "awi"],
    "kimi": ["kimi", "doctor"],
    "opencode": ["opencode", "mcp", "list"],
    "pi": ["pi", "list"],
    "cursor": ["cursor-agent", "mcp", "list"],
    "qwen": ["qwen", "mcp", "list"],
    "cline": ["cline", "config", "mcp", "--json"],
    "amazon-q": ["qchat", "mcp", "list"],
    "crush": ["crush", "--help"],
}

VERSION_ARGS = {
    "codex": ["codex", "--version"],
    "gemini": ["gemini", "--version"],
    "claude": ["claude", "--version"],
    "copilot": ["copilot", "--version"],
    "kimi": ["kimi", "--version"],
    "opencode": ["opencode", "--version"],
    "pi": ["pi", "--version"],
    "cursor": ["cursor-agent", "--version"],
    "qwen": ["qwen", "--version"],
    "cline": ["cline", "--version"],
    "amazon-q": ["qchat", "--version"],
    "crush": ["crush", "--version"],
}

GUI_MARKERS = [
    ".trae-cn",
    ".zcode",
    ".codeium/windsurf",
]

MAX_CAPTURE_CHARS = 16_000


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--awi-bin", type=Path, required=True)
    parser.add_argument("--index-dir", type=Path, required=True)
    parser.add_argument("--socket", type=Path, required=True)
    parser.add_argument("--sandbox", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--path-prefix", type=Path, action="append", default=[])
    return parser.parse_args()


def run(
    command: list[str],
    *,
    env: dict[str, str],
    cwd: Path,
    timeout: int = 45,
) -> dict[str, Any]:
    started = time.monotonic()
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
        return {
            "command": command,
            "returncode": result.returncode,
            "duration_ms": round((time.monotonic() - started) * 1000, 3),
            "stdout": result.stdout[-MAX_CAPTURE_CHARS:],
            "stderr": result.stderr[-MAX_CAPTURE_CHARS:],
            "timed_out": False,
        }
    except subprocess.TimeoutExpired as error:
        return {
            "command": command,
            "returncode": None,
            "duration_ms": round((time.monotonic() - started) * 1000, 3),
            "stdout": text(error.stdout)[-MAX_CAPTURE_CHARS:],
            "stderr": text(error.stderr)[-MAX_CAPTURE_CHARS:],
            "timed_out": True,
        }


def text(value: str | bytes | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode(errors="replace")
    return value


def seed_json(path: Path) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text('{\n  "preserved": true\n}\n')
    path.chmod(0o600)


def prepare_sandbox(args: argparse.Namespace) -> tuple[Path, Path, dict[str, str]]:
    sandbox = args.sandbox.resolve()
    if not sandbox.is_relative_to("/tmp"):
        raise SystemExit("--sandbox must be under worker-local /tmp")
    shutil.rmtree(sandbox, ignore_errors=True)
    home = sandbox / "home"
    config_home = sandbox / "config"
    workspace = sandbox / "workspace"
    home.mkdir(parents=True)
    config_home.mkdir(parents=True)
    (workspace / "src").mkdir(parents=True)
    (workspace / "src/example.rs").write_text(
        "pub fn harness_probe() -> bool { true }\n"
    )
    for marker in GUI_MARKERS:
        (home / marker).mkdir(parents=True, exist_ok=True)
    (config_home / "zed").mkdir(parents=True, exist_ok=True)

    for client, relative in CONFIG_PATHS.items():
        if client == "codex":
            continue
        seed_json(home / relative)
    codex_config = home / CONFIG_PATHS["codex"]
    codex_config.parent.mkdir(parents=True, exist_ok=True)
    codex_config.write_text("preserved = true\n")
    codex_config.chmod(0o600)
    for relative in XDG_CONFIG_PATHS.values():
        seed_json(config_home / relative)
    seed_json(workspace / ".trae/mcp.json")

    path_entries = [str(path) for path in args.path_prefix]
    path_entries.extend(
        [
            str(home / ".local/bin"),
            "/home/tiger/.local/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/bin",
        ]
    )
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home),
            "XDG_CONFIG_HOME": str(config_home),
            "CODEX_HOME": str(home / ".codex"),
            "COPILOT_HOME": str(home / ".copilot"),
            "KIMI_CODE_HOME": str(home / ".kimi-code"),
            "PI_CODING_AGENT_DIR": str(home / ".pi/agent"),
            "OPENCODE_CONFIG": str(config_home / "opencode/opencode.json"),
            "PATH": os.pathsep.join(path_entries),
            "NO_COLOR": "1",
            "CI": "1",
        }
    )
    return home, workspace, env


def integrate_command(args: argparse.Namespace, workspace: Path) -> list[str]:
    command = [
        str(args.awi_bin),
        "--index-dir",
        str(args.index_dir),
        "--socket",
        str(args.socket),
        "integrate",
        "--client",
        "all",
        "--server-command",
        str(args.awi_bin),
    ]
    for value in (
        "--index-dir",
        str(args.index_dir),
        "--socket",
        str(args.socket),
        "mcp",
    ):
        command.extend(["--server-arg", value])
    command.extend(["--project-root", str(workspace), "--json"])
    return command


def config_record(path: Path) -> dict[str, Any]:
    record: dict[str, Any] = {
        "path": str(path),
        "exists": path.is_file(),
    }
    if not path.is_file():
        return record
    metadata = path.stat()
    value = path.read_text(errors="replace")
    record.update(
        {
            "mode": stat.S_IMODE(metadata.st_mode),
            "size_bytes": metadata.st_size,
            "preserved": "preserved" in value,
            "mentions_awi": "awi" in value,
            "content": value[-MAX_CAPTURE_CHARS:],
        }
    )
    return record


def main() -> None:
    args = parse_args()
    home, workspace, env = prepare_sandbox(args)
    pi_install = run(
        ["pi", "install", "npm:pi-mcp-adapter@2.36.0"],
        env=env,
        cwd=workspace,
        timeout=120,
    )
    if pi_install["returncode"] != 0:
        pi_npm_root = home / ".pi/agent/npm"
        pi_npm_install = run(
            [
                "npm",
                "install",
                "--prefix",
                str(pi_npm_root),
                "--no-audit",
                "--no-fund",
                "pi-mcp-adapter@2.36.0",
            ],
            env=env,
            cwd=workspace,
            timeout=120,
        )
        pi_local_install = run(
            ["pi", "install", str(pi_npm_root / "node_modules/pi-mcp-adapter")],
            env=env,
            cwd=workspace,
            timeout=120,
        )
    else:
        pi_npm_install = None
        pi_local_install = None
    command = integrate_command(args, workspace)
    first = run(command, env=env, cwd=workspace, timeout=120)
    second = run(command, env=env, cwd=workspace, timeout=120)

    configs = {
        client: config_record(home / relative)
        for client, relative in CONFIG_PATHS.items()
    }
    configs.update(
        {
            client: config_record(Path(env["XDG_CONFIG_HOME"]) / relative)
            for client, relative in XDG_CONFIG_PATHS.items()
        }
    )
    configs["trae"] = config_record(workspace / ".trae/mcp.json")
    configs["crush_actual_default"] = config_record(
        home / ".local/share/crush/crush.json"
    )

    versions = {
        client: run(command, env=env, cwd=workspace, timeout=30)
        for client, command in VERSION_ARGS.items()
    }
    probes = {
        client: run(command, env=env, cwd=workspace, timeout=60)
        for client, command in PROBES.items()
    }
    output = {
        "worker": subprocess.run(
            ["hostname"], capture_output=True, text=True, check=True
        ).stdout.strip(),
        "clients": CLIENTS,
        "environment": {
            "home": str(home),
            "xdg_config_home": env["XDG_CONFIG_HOME"],
            "workspace": str(workspace),
            "path": env["PATH"],
        },
        "pi_adapter_install": pi_install,
        "pi_adapter_fallback_npm": pi_npm_install,
        "pi_adapter_fallback_local": pi_local_install,
        "integrate_first": first,
        "integrate_second": second,
        "configs": configs,
        "versions": versions,
        "probes": probes,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n")
    print(json.dumps(output, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
