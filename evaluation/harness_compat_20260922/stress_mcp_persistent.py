#!/usr/bin/env python3
"""Run the MCP stress suite with realistic persistent request/response clients."""

from __future__ import annotations

import json
import selectors
import subprocess
import time
from typing import Any

import stress_mcp as base


def read_response(process: subprocess.Popen[str], timeout_seconds: int) -> str:
    assert process.stdout is not None
    selector = selectors.DefaultSelector()
    try:
        selector.register(process.stdout, selectors.EVENT_READ)
        if not selector.select(timeout_seconds):
            raise TimeoutError(f"MCP response timed out after {timeout_seconds}s")
        line = process.stdout.readline()
        if not line:
            raise TimeoutError("MCP process closed stdout without a response")
        return line
    finally:
        selector.close()


def run_mcp_client(args: Any, client_id: int) -> dict[str, Any]:
    command = [
        str(args.awi_bin),
        "--index-dir",
        str(args.index_dir),
        "--socket",
        str(args.socket),
        "mcp",
        "--audit-log",
        str(args.audit_log),
    ]
    process = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    started = time.monotonic()
    responses: list[str] = []
    try:
        assert process.stdin is not None
        initialize, notification, *calls = base.client_payload(
            args.root, client_id, args.calls_per_client
        ).splitlines()
        process.stdin.write(initialize + "\n")
        process.stdin.flush()
        responses.append(read_response(process, 120))
        process.stdin.write(notification + "\n")
        process.stdin.flush()
        for call in calls:
            process.stdin.write(call + "\n")
            process.stdin.flush()
            responses.append(read_response(process, 120))
        process.stdin.close()
        returncode = process.wait(timeout=30)
        assert process.stderr is not None
        stderr = process.stderr.read()
        parsed = []
        parse_errors = []
        for response in responses:
            try:
                parsed.append(json.loads(response))
            except json.JSONDecodeError as error:
                parse_errors.append(str(error))
        calls = [
            response
            for response in parsed
            if isinstance(response.get("id"), int) and response["id"] >= 2
        ]
        return {
            "returncode": returncode,
            "duration_ms": round((time.monotonic() - started) * 1000, 3),
            "responses": len(parsed),
            "calls": len(calls),
            "tool_errors": sum(
                response.get("result", {}).get("isError") is True for response in calls
            ),
            "parse_errors": parse_errors,
            "stdout_bytes": sum(len(response.encode()) + 1 for response in responses),
            "stderr": stderr[-4_000:],
            "timed_out": False,
        }
    except (subprocess.TimeoutExpired, TimeoutError) as error:
        process.kill()
        try:
            process.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass
        stderr = process.stderr.read() if process.stderr is not None else ""
        return {
            "returncode": None,
            "duration_ms": round((time.monotonic() - started) * 1000, 3),
            "responses": len(responses),
            "calls": 0,
            "tool_errors": 0,
            "parse_errors": [str(error)],
            "stdout_bytes": sum(len(response.encode()) + 1 for response in responses),
            "stderr": stderr[-4_000:],
            "timed_out": True,
        }


if __name__ == "__main__":
    base.run_mcp_client = run_mcp_client
    base.main()
