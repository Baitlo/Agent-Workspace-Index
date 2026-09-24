#!/usr/bin/env python3
"""Run repeatable Gemini headless prompts against an isolated AWI config."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import time
from pathlib import Path

PROMPTS = [
    "Use only AWI workspace_search. Find the Rust file that implements integrate_cline and report its absolute path.",
    "Use only AWI workspace_search. Find the documentation mentioning compact_v3 and report the path.",
    "Use AWI workspace_search with agent_memory and context_path /home/tiger/Projects/Bona. What commit added cross-agent memory?",
    "Use only AWI workspace_search. Find the test for snapshot refresh not blocking the request path and report the path.",
    "Use only AWI workspace_search. Find the Cline integration implementation and state its current config path.",
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--gemini-bin", type=Path, required=True)
    parser.add_argument("--source-env", type=Path, required=True)
    parser.add_argument("--source-settings", type=Path, required=True)
    parser.add_argument("--mcp-settings", type=Path, required=True)
    parser.add_argument("--sandbox", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--cwd", type=Path, required=True)
    parser.add_argument("--limit", type=int)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    sandbox = args.sandbox.resolve()
    if not sandbox.is_relative_to("/tmp"):
        raise SystemExit("--sandbox must be under worker-local /tmp")
    shutil.rmtree(sandbox, ignore_errors=True)
    gemini_home = sandbox / "home/.gemini"
    gemini_home.mkdir(parents=True)
    shutil.copy2(args.source_env, gemini_home / ".env")
    source = json.loads(args.source_settings.read_text())
    mcp = json.loads(args.mcp_settings.read_text())
    source["mcpServers"] = mcp["mcpServers"]
    (gemini_home / "settings.json").write_text(json.dumps(source, indent=2) + "\n")
    gemini_home.chmod(0o700)
    (gemini_home / ".env").chmod(0o600)
    (gemini_home / "settings.json").chmod(0o600)

    args.output.mkdir(parents=True, exist_ok=True)
    environment = os.environ.copy()
    environment.update(
        {
            "HOME": str(sandbox / "home"),
            "NO_COLOR": "1",
            "CI": "1",
        }
    )
    results = []
    for index, prompt in enumerate(PROMPTS[: args.limit], 1):
        command = [
            str(args.gemini_bin),
            "--skip-trust",
            "--approval-mode",
            "default",
            "--allowed-mcp-server-names",
            "awi",
            "--output-format",
            "stream-json",
            "--prompt",
            prompt,
        ]
        started = time.monotonic()
        result = subprocess.run(
            command,
            cwd=args.cwd,
            env=environment,
            capture_output=True,
            text=True,
            timeout=240,
            check=False,
        )
        (args.output / f"run-{index}.jsonl").write_text(result.stdout)
        (args.output / f"run-{index}.stderr").write_text(result.stderr)
        tool_calls = []
        for line in result.stdout.splitlines():
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if "tool" in str(event).lower() and "workspace_search" in str(event):
                tool_calls.append(event)
        results.append(
            {
                "index": index,
                "returncode": result.returncode,
                "duration_ms": round((time.monotonic() - started) * 1000, 3),
                "awi_tool_events": len(tool_calls),
                "stderr_tail": result.stderr[-2_000:],
            }
        )
    summary = {
        "worker": subprocess.run(
            ["hostname"], capture_output=True, text=True, check=True
        ).stdout.strip(),
        "runs": len(results),
        "successful": sum(result["returncode"] == 0 for result in results),
        "runs_with_awi_tool_events": sum(
            result["awi_tool_events"] > 0 for result in results
        ),
        "results": results,
    }
    (args.output / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n"
    )
    print(json.dumps(summary, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
