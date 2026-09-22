#!/usr/bin/env python3
"""Stress AWI's MCP adapter and daemon protocol from a CPU worker."""

from __future__ import annotations

import argparse
import concurrent.futures
import json
import socket
import statistics
import subprocess
import time
from pathlib import Path
from typing import Any

QUERIES = [
    "snapshot refresh",
    "workspace_search integration",
    "agent memory project",
    "DuckDB query timeout",
]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--awi-bin", type=Path, required=True)
    parser.add_argument("--index-dir", type=Path, required=True)
    parser.add_argument("--socket", type=Path, required=True)
    parser.add_argument("--audit-log", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--daemon-pid", type=int)
    parser.add_argument("--clients-per-level", type=int, default=128)
    parser.add_argument("--calls-per-client", type=int, default=4)
    parser.add_argument("--levels", default="1,8,32,64")
    parser.add_argument("--disconnects", type=int, default=200)
    return parser.parse_args()


def percentile(values: list[float], quantile: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, int(len(ordered) * quantile + 0.999) - 1))
    return ordered[index]


def request_line(request_id: int, method: str, params: dict[str, Any]) -> str:
    return json.dumps(
        {
            "jsonrpc": "2.0",
            "id": request_id,
            "method": method,
            "params": params,
        },
        separators=(",", ":"),
    )


def client_payload(root: Path, client_id: int, calls: int) -> str:
    lines = [
        request_line(
            1,
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {
                    "name": f"awi-stress-{client_id}",
                    "version": "1",
                },
            },
        ),
        json.dumps(
            {
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {},
            },
            separators=(",", ":"),
        ),
    ]
    for call_index in range(calls):
        lines.append(
            request_line(
                call_index + 2,
                "tools/call",
                {
                    "name": "workspace_search",
                    "arguments": {
                        "query": QUERIES[(client_id + call_index) % len(QUERIES)],
                        "limit": 8,
                        "roots": [str(root)],
                    },
                },
            )
        )
    return "\n".join(lines) + "\n"


def run_mcp_client(args: argparse.Namespace, client_id: int) -> dict[str, Any]:
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
    started = time.monotonic()
    try:
        result = subprocess.run(
            command,
            input=client_payload(args.root, client_id, args.calls_per_client),
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
        responses = []
        parse_errors = []
        for line in result.stdout.splitlines():
            try:
                responses.append(json.loads(line))
            except json.JSONDecodeError as error:
                parse_errors.append(str(error))
        calls = [
            response
            for response in responses
            if isinstance(response.get("id"), int) and response["id"] >= 2
        ]
        tool_errors = sum(
            response.get("result", {}).get("isError") is True for response in calls
        )
        return {
            "returncode": result.returncode,
            "duration_ms": round((time.monotonic() - started) * 1000, 3),
            "responses": len(responses),
            "calls": len(calls),
            "tool_errors": tool_errors,
            "parse_errors": parse_errors,
            "stdout_bytes": len(result.stdout.encode()),
            "stderr": result.stderr[-4_000:],
            "timed_out": False,
        }
    except subprocess.TimeoutExpired as error:
        return {
            "returncode": None,
            "duration_ms": round((time.monotonic() - started) * 1000, 3),
            "responses": 0,
            "calls": 0,
            "tool_errors": 0,
            "parse_errors": [],
            "stdout_bytes": len(bytes_value(error.stdout)),
            "stderr": text_value(error.stderr)[-4_000:],
            "timed_out": True,
        }


def text_value(value: str | bytes | None) -> str:
    if value is None:
        return ""
    if isinstance(value, bytes):
        return value.decode(errors="replace")
    return value


def bytes_value(value: str | bytes | None) -> bytes:
    if value is None:
        return b""
    if isinstance(value, bytes):
        return value
    return value.encode()


def run_level(
    args: argparse.Namespace, concurrency: int, first_id: int
) -> dict[str, Any]:
    started = time.monotonic()
    with concurrent.futures.ThreadPoolExecutor(max_workers=concurrency) as executor:
        results = list(
            executor.map(
                lambda client_id: run_mcp_client(args, client_id),
                range(first_id, first_id + args.clients_per_level),
            )
        )
    durations = [result["duration_ms"] for result in results]
    return {
        "concurrency": concurrency,
        "clients": len(results),
        "calls_expected": len(results) * args.calls_per_client,
        "calls_observed": sum(result["calls"] for result in results),
        "nonzero_exits": sum(result["returncode"] != 0 for result in results),
        "timeouts": sum(result["timed_out"] for result in results),
        "tool_errors": sum(result["tool_errors"] for result in results),
        "parse_errors": sum(len(result["parse_errors"]) for result in results),
        "stdout_bytes": sum(result["stdout_bytes"] for result in results),
        "wall_ms": round((time.monotonic() - started) * 1000, 3),
        "client_ms": {
            "mean": round(statistics.fmean(durations), 3),
            "p50": round(percentile(durations, 0.50), 3),
            "p95": round(percentile(durations, 0.95), 3),
            "p99": round(percentile(durations, 0.99), 3),
            "max": round(max(durations), 3),
        },
        "failures": [
            result
            for result in results
            if result["returncode"] != 0
            or result["timed_out"]
            or result["parse_errors"]
            or result["tool_errors"]
            or result["calls"] != args.calls_per_client
        ][:20],
    }


def edge_payload(root: Path) -> str:
    oversized_query = "x" * 4_097
    calls = [
        {
            "query": "",
            "limit": 8,
        },
        {
            "query": "snapshot",
            "limit": 0,
        },
        {
            "query": "snapshot",
            "limit": 51,
        },
        {
            "query": oversized_query,
            "limit": 8,
        },
        {
            "query": "snapshot",
            "limit": 50,
            "roots": [str(root)],
        },
    ]
    lines = [
        request_line(
            1,
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "awi-edge", "version": "1"},
            },
        ),
        json.dumps(
            {
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {},
            },
            separators=(",", ":"),
        ),
    ]
    for offset, arguments in enumerate(calls, 2):
        lines.append(
            request_line(
                offset,
                "tools/call",
                {"name": "workspace_search", "arguments": arguments},
            )
        )
    lines.extend(
        [
            request_line(
                7,
                "tools/call",
                {
                    "name": "workspace_inspect",
                    "arguments": {
                        "path": str(root / "does-not-exist"),
                        "max_chars": 65_537,
                    },
                },
            ),
            request_line(
                8,
                "tools/call",
                {
                    "name": "workspace_query",
                    "arguments": {
                        "sql": "DELETE FROM data_0",
                        "roots": [str(root)],
                        "inputs": [],
                    },
                },
            ),
        ]
    )
    return "\n".join(lines) + "\n"


def run_edge_cases(args: argparse.Namespace) -> dict[str, Any]:
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
    result = subprocess.run(
        command,
        input=edge_payload(args.root),
        capture_output=True,
        text=True,
        timeout=120,
        check=False,
    )
    responses = [json.loads(line) for line in result.stdout.splitlines()]
    compact = next(response for response in responses if response.get("id") == 6)
    return {
        "returncode": result.returncode,
        "responses": responses,
        "stderr": result.stderr[-4_000:],
        "limit_50": {
            "returned": compact["result"]["structuredContent"]["returned"],
            "effective_limit": compact["result"]["structuredContent"][
                "effective_limit"
            ],
            "limit_compacted": compact["result"]["structuredContent"][
                "limit_compacted"
            ],
        },
    }


def socket_exchange(path: Path, payload: bytes) -> dict[str, Any]:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(10)
        client.connect(str(path))
        client.sendall(payload)
        response = b""
        while not response.endswith(b"\n"):
            chunk = client.recv(65_536)
            if not chunk:
                break
            response += chunk
    try:
        decoded = json.loads(response)
    except json.JSONDecodeError:
        decoded = None
    return {
        "response_bytes": len(response),
        "response": decoded,
    }


def disconnect_once(path: Path) -> None:
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(10)
    client.connect(str(path))
    client.sendall(b'{"protocol_version":3,"request":{"method":"ping"}}\n')
    client.shutdown(socket.SHUT_RDWR)
    client.close()


def run_daemon_edges(args: argparse.Namespace) -> dict[str, Any]:
    with concurrent.futures.ThreadPoolExecutor(max_workers=64) as executor:
        list(
            executor.map(
                lambda _: disconnect_once(args.socket), range(args.disconnects)
            )
        )
    malformed = socket_exchange(args.socket, b"not-json\n")
    oversized = socket_exchange(args.socket, b"x" * (1024 * 1024 + 1) + b"\n")
    ping = subprocess.run(
        [
            str(args.awi_bin),
            "--index-dir",
            str(args.index_dir),
            "--socket",
            str(args.socket),
            "ping",
            "--json",
        ],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    return {
        "disconnects": args.disconnects,
        "malformed": malformed,
        "oversized": oversized,
        "post_edge_ping": {
            "returncode": ping.returncode,
            "stdout": ping.stdout,
            "stderr": ping.stderr,
        },
    }


def process_rss_kib(pid: int | None) -> int | None:
    if pid is None:
        return None
    try:
        for line in Path(f"/proc/{pid}/status").read_text().splitlines():
            if line.startswith("VmRSS:"):
                return int(line.split()[1])
    except (FileNotFoundError, PermissionError, ValueError):
        return None
    return None


def audit_summary(path: Path) -> dict[str, Any]:
    records = []
    if path.is_file():
        for line in path.read_text(errors="replace").splitlines():
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    durations = [
        float(record["duration_ms"])
        for record in records
        if isinstance(record.get("duration_ms"), (int, float))
    ]
    return {
        "records": len(records),
        "statuses": {
            status: sum(record.get("status") == status for record in records)
            for status in sorted({record.get("status") for record in records})
            if status is not None
        },
        "duration_ms": {
            "p50": round(percentile(durations, 0.50), 3),
            "p95": round(percentile(durations, 0.95), 3),
            "p99": round(percentile(durations, 0.99), 3),
            "max": round(max(durations), 3) if durations else 0,
        },
        "responses_over_100k": sum(
            int(record.get("response_bytes", 0)) > 100 * 1024 for record in records
        ),
        "schema_versions": sorted(
            {
                record.get("schema_version")
                for record in records
                if record.get("schema_version") is not None
            }
        ),
    }


def main() -> None:
    args = parse_args()
    levels = [int(value) for value in args.levels.split(",") if value]
    args.audit_log.parent.mkdir(parents=True, exist_ok=True)
    args.audit_log.unlink(missing_ok=True)
    before_rss = process_rss_kib(args.daemon_pid)
    level_results = []
    next_id = 0
    for level in levels:
        level_results.append(run_level(args, level, next_id))
        next_id += args.clients_per_level
    edges = run_edge_cases(args)
    daemon_edges = run_daemon_edges(args)
    output = {
        "worker": subprocess.run(
            ["hostname"], capture_output=True, text=True, check=True
        ).stdout.strip(),
        "levels": level_results,
        "edge_cases": edges,
        "daemon_edges": daemon_edges,
        "daemon_rss_kib": {
            "before": before_rss,
            "after": process_rss_kib(args.daemon_pid),
        },
        "audit": audit_summary(args.audit_log),
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(output, indent=2, sort_keys=True) + "\n")
    print(json.dumps(output, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
