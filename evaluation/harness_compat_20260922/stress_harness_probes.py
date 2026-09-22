#!/usr/bin/env python3
"""Repeat native harness MCP discovery commands under bounded concurrency."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import os
import subprocess
import time
from pathlib import Path
from typing import Any

PROBES = {
    "codex": (["codex", "mcp", "get", "awi", "--json"], ["awi"]),
    "gemini": (["gemini", "mcp", "list"], ["awi"]),
    "claude": (["claude", "mcp", "get", "awi"], ["awi", "connected"]),
    "copilot": (["copilot", "mcp", "get", "awi"], ["awi", "enabled"]),
    "opencode": (["opencode", "mcp", "list"], ["awi", "connected"]),
    "pi": (["pi", "list"], ["pi-mcp-adapter"]),
    "cursor": (["cursor-agent", "mcp", "list"], ["awi", "ready"]),
    "qwen": (["qwen", "mcp", "list"], ["awi", "connected"]),
    "cline": (["cline", "config", "mcp", "--json"], ["awi"]),
    "amazon-q": (["qchat", "mcp", "list"], ["awi"]),
}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repetitions", type=int, default=20)
    parser.add_argument("--concurrency", type=int, default=20)
    return parser.parse_args()


def percentile(values: list[float], quantile: float) -> float:
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, int(len(ordered) * quantile + 0.999) - 1))
    return ordered[index]


def run_probe(
    client: str,
    repetition: int,
    command: list[str],
    expected: list[str],
    env: dict[str, str],
    cwd: Path,
) -> dict[str, Any]:
    started = time.monotonic()
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        combined = f"{result.stdout}\n{result.stderr}".lower()
        return {
            "client": client,
            "repetition": repetition,
            "returncode": result.returncode,
            "duration_ms": round((time.monotonic() - started) * 1000, 3),
            "passed": result.returncode == 0
            and all(token in combined for token in expected),
            "stdout": result.stdout[-2_000:],
            "stderr": result.stderr[-2_000:],
            "timed_out": False,
        }
    except subprocess.TimeoutExpired as error:
        return {
            "client": client,
            "repetition": repetition,
            "returncode": None,
            "duration_ms": round((time.monotonic() - started) * 1000, 3),
            "passed": False,
            "stdout": decode(error.stdout)[-2_000:],
            "stderr": decode(error.stderr)[-2_000:],
            "timed_out": True,
        }


def decode(value: str | bytes | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode(errors="replace")
    return value


def main() -> None:
    args = parse_args()
    matrix = json.loads(args.matrix.read_text())
    environment = matrix["environment"]
    home = Path(environment["home"])
    env = os.environ.copy()
    env.update(
        {
            "HOME": str(home),
            "XDG_CONFIG_HOME": environment["xdg_config_home"],
            "CODEX_HOME": str(home / ".codex"),
            "COPILOT_HOME": str(home / ".copilot"),
            "KIMI_CODE_HOME": str(home / ".kimi-code"),
            "PI_CODING_AGENT_DIR": str(home / ".pi/agent"),
            "OPENCODE_CONFIG": str(
                Path(environment["xdg_config_home"]) / "opencode/opencode.json"
            ),
            "PATH": environment["path"],
            "NO_COLOR": "1",
            "CI": "1",
        }
    )
    cwd = Path(environment["workspace"])
    work = [
        (client, repetition, command, expected)
        for repetition in range(1, args.repetitions + 1)
        for client, (command, expected) in PROBES.items()
    ]
    started = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(
        max_workers=args.concurrency
    ) as executor:
        futures = [
            executor.submit(run_probe, client, repetition, command, expected, env, cwd)
            for client, repetition, command, expected in work
        ]
        results = [future.result() for future in futures]

    clients = {}
    for client in PROBES:
        values = [result for result in results if result["client"] == client]
        durations = [result["duration_ms"] for result in values]
        clients[client] = {
            "runs": len(values),
            "passed": sum(result["passed"] for result in values),
            "timeouts": sum(result["timed_out"] for result in values),
            "nonzero_exits": sum(result["returncode"] != 0 for result in values),
            "duration_ms": {
                "p50": round(percentile(durations, 0.50), 3),
                "p95": round(percentile(durations, 0.95), 3),
                "max": round(max(durations), 3),
            },
            "failures": [result for result in values if not result["passed"]][:5],
        }
    output = {
        "worker": matrix["worker"],
        "repetitions": args.repetitions,
        "concurrency": args.concurrency,
        "runs": len(results),
        "passed": sum(result["passed"] for result in results),
        "wall_ms": round((time.monotonic() - started) * 1000, 3),
        "clients": clients,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n")
    print(json.dumps(output, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
