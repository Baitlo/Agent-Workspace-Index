#!/usr/bin/env python3
"""Probe an installed MCP command; protocol visibility is not GUI receipt."""

import argparse
import json
import os
import selectors
import subprocess
import time
from pathlib import Path


def probe(command, query, context, structured, startup_timeout=660):
    environment = {
        **os.environ,
        "AWI_MCP_SYNTHETIC": "true",
        "AWI_MCP_CLIENT_NAME": "awi-delivery-probe",
    }
    process = subprocess.Popen(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env=environment,
        bufsize=0,
    )
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    pending = bytearray()
    sequence = 0

    def send(message):
        process.stdin.write(json.dumps(message).encode() + b"\n")
        process.stdin.flush()

    def request(method, params, timeout=60):
        nonlocal sequence
        sequence += 1
        send({"jsonrpc": "2.0", "id": sequence, "method": method, "params": params})
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            while b"\n" in pending:
                line, _, rest = pending.partition(b"\n")
                pending[:] = rest
                value = json.loads(line)
                if value.get("id") == sequence:
                    if "error" in value:
                        raise RuntimeError(value["error"])
                    return value["result"]
            if selector.select(min(1, max(0, deadline - time.monotonic()))):
                chunk = os.read(process.stdout.fileno(), 65536)
                if not chunk:
                    raise RuntimeError("MCP exited before response")
                pending.extend(chunk)
        raise TimeoutError(method)

    try:
        capabilities = (
            {"experimental": {"awi": {"response_channel": "structured"}}}
            if structured
            else {}
        )
        initialize = request(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": capabilities,
                "clientInfo": {"name": "awi-delivery-probe", "version": "1"},
            },
            timeout=startup_timeout,
        )
        send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        request("ping", {})
        tools = request("tools/list", {})
        arguments = {"query": query, "limit": 5}
        if context:
            arguments["context_path"] = context
        search = request(
            "tools/call", {"name": "workspace_search", "arguments": arguments}
        )
        payload = search.get("structuredContent", {})
        text = "\n".join(block.get("text", "") for block in search.get("content", []))
        hits = payload.get("hits", [])
        previews = [hit["preview"] for hit in hits if hit.get("preview")]
        inspect = None
        if hits:
            inspect = request(
                "tools/call",
                {
                    "name": "workspace_inspect",
                    "arguments": {
                        "path": hits[0]["path"],
                        "max_lines": 80,
                        "search_id": payload.get("search_id"),
                    },
                },
            )
        body = (inspect or {}).get("structuredContent", {})
        body_text = "\n".join(
            b.get("text", "") for b in (inspect or {}).get("content", [])
        )
        excerpt = (
            body.get("content", {}).get("text", "")
            if isinstance(body.get("content"), dict)
            else ""
        )
        runtime = payload.get("delivery", {}).get("runtime", {})
        reader = runtime.get("reader_at_initialize") or {}
        checks = {
            "three_tools": {tool["name"] for tool in tools["tools"]}
            == {"workspace_search", "workspace_inspect", "workspace_query"},
            "search_succeeded": not search.get("isError", False),
            "nonempty_hits": bool(hits),
            "runtime_build_present": bool(runtime.get("adapter_build_id")),
            "reader_build_matches": bool(runtime.get("adapter_build_id"))
            and runtime["adapter_build_id"] == reader.get("build_id"),
            "adapter_not_replaced": runtime.get("adapter_executable_replaced") is False,
            "channel_negotiated": payload.get("delivery", {})
            .get("runtime", {})
            .get("channel")
            == ("structured" if structured else "text_and_structured"),
            "previews_visible": all(preview in text for preview in previews)
            if not structured
            else bool(previews),
            "search_id_visible": bool(payload.get("search_id"))
            and (structured or payload["search_id"] in text),
            "inspect_succeeded": inspect is not None
            and not inspect.get("isError", False),
            "inspect_excerpt_visible": bool(excerpt)
            and (structured or excerpt in body_text),
        }
        return {
            "mode": "structured" if structured else "both",
            "checks": checks,
            "passed": all(checks.values()),
            "initialize": initialize,
            "delivery": payload.get("delivery"),
            "hits": len(hits),
            "nonempty_previews": len(previews),
            "text_bytes": len(text.encode()),
            "search_response_bytes": len(json.dumps(search).encode()),
            "inspect_excerpt_chars": len(excerpt),
            "evidence_level": "standalone_stdio_protocol",
            "actual_gui_receipt": "unverified",
        }
    finally:
        process.stdin.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        process.stdout.close()
        selector.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--query", required=True)
    parser.add_argument("--context")
    parser.add_argument(
        "--startup-timeout",
        type=int,
        default=660,
        help="Initialize deadline in seconds; permits cold snapshot startup",
    )
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if args.startup_timeout <= 0:
        parser.error("--startup-timeout must be positive")
    if not command:
        parser.error("MCP command is required after --")
    results = [
        probe(command, args.query, args.context, structured, args.startup_timeout)
        for structured in (False, True)
    ]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.fchmod(fd, 0o600)
    with os.fdopen(fd, "w") as handle:
        json.dump(results, handle, ensure_ascii=False, indent=2)
        handle.write("\n")
    print(json.dumps(results, ensure_ascii=False, indent=2))
    return 0 if all(result["passed"] for result in results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
