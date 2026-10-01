#!/usr/bin/env python3
"""Replay observed searches over copied immutable shards; no Agent-helpfulness claim."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import select
import shutil
import subprocess
import time
from pathlib import Path


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.fchmod(fd, 0o600)
    with os.fdopen(fd, "w") as handle:
        json.dump(value, handle, ensure_ascii=False, indent=2)
        handle.write("\n")


def pin(source, sandbox):
    """Copy current generations; never follow mutable current.json in the replay."""
    dest = sandbox / "pinned"
    manifest = dest / "pin.json"
    if manifest.exists():
        return dest, json.loads(manifest.read_text())
    collection = json.loads((source / "collection.json").read_text())
    identities = []
    for shard in collection["shards"]:
        origin = (source / shard["source"]).resolve()
        pointer = json.loads((origin / "current.json").read_text())
        name = pointer["directory"]
        if Path(name).name != name:
            raise ValueError("invalid snapshot directory")
        target = dest / shard["name"]
        print(f"Pinning {shard['name']} generation {pointer['generation']}", flush=True)
        shutil.copytree(origin / "generations" / name, target / "generations" / name)
        save(target / "current.json", pointer)
        identities.append(
            {"name": shard["name"], "source": str(origin), "pointer": pointer}
        )
        shard["source"] = str(target)
    save(dest / "collection.json", collection)
    save(manifest, identities)
    return dest, identities


class Mcp:
    def __init__(self, binary, index, socket, log):
        env = {
            **os.environ,
            "AWI_MCP_SYNTHETIC": "true",
            "AWI_MCP_CLIENT_NAME": "awi-outcome-replay",
        }
        self.process = subprocess.Popen(
            [str(binary), "--index-dir", str(index), "--socket", str(socket), "mcp"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=log,
            env=env,
        )
        self.sequence = 0
        self.request(
            "initialize",
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "awi-outcome-replay", "version": "1"},
            },
        )
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def send(self, message):
        self.process.stdin.write(json.dumps(message).encode() + b"\n")
        self.process.stdin.flush()

    def request(self, method, params):
        self.sequence += 1
        self.send(
            {"jsonrpc": "2.0", "id": self.sequence, "method": method, "params": params}
        )
        if not select.select([self.process.stdout], [], [], 90)[0]:
            raise TimeoutError(f"MCP request timed out: {method}")
        response = json.loads(self.process.stdout.readline())
        if response.get("id") != self.sequence:
            raise ValueError("unexpected MCP response id")
        return response

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()


def stop(process):
    process.terminate()
    try:
        process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def run_arm(binary, name, repetition, source, sandbox, calls, output):
    index = sandbox / name
    socket = sandbox / f"{name}.sock"
    log_path = output.parent / f"{name}-{repetition}.log"
    with log_path.open("ab") as log:
        daemon = subprocess.Popen(
            [
                str(binary),
                "--index-dir",
                str(index),
                "--socket",
                str(socket),
                "serve",
                "--snapshot-source",
                str(source),
                "--snapshot-poll-ms",
                "3600000",
            ],
            stdout=log,
            stderr=log,
        )
        client = None
        try:
            deadline = time.monotonic() + 600
            while not socket.exists():
                if daemon.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError(f"{name} daemon failed to start; see {log_path}")
                time.sleep(0.2)
            client = Mcp(binary, index, socket, log)
            results = []
            for i, arguments in enumerate(calls):
                started = time.monotonic()
                result = client.request(
                    "tools/call", {"name": "workspace_search", "arguments": arguments}
                )
                elapsed = (time.monotonic() - started) * 1000
                payload = result.get("result", {})
                structured = payload.get("structuredContent", {})
                text = "\n".join(
                    block.get("text", "") for block in payload.get("content", [])
                )
                hits = structured.get("hits", [])
                results.append(
                    {
                        "arguments": arguments,
                        "duration_ms": elapsed,
                        "error": result.get("error") or structured.get("error"),
                        "hits": hits,
                        "text_bytes": len(text.encode()),
                        "response_bytes": len(json.dumps(result).encode()),
                        "nonempty_previews": sum(
                            bool(hit.get("preview")) for hit in hits
                        ),
                        "visible_previews": sum(
                            bool(hit.get("preview")) and hit["preview"] in text
                            for hit in hits
                        ),
                        "search_id_visible": bool(structured.get("search_id"))
                        and structured["search_id"] in text,
                        "text_truncated": structured.get("delivery", {}).get(
                            "text_truncated"
                        ),
                    }
                )
                if i % 20 == 0:
                    print(f"{name}/{repetition}: {i + 1}/{len(calls)}", flush=True)
            save(output, results)
        finally:
            if client:
                client.close()
            stop(daemon)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--sandbox", type=Path, required=True)
    parser.add_argument("--observations", type=Path, required=True)
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--pin-only", action="store_true")
    parser.add_argument(
        "--gold", type=Path, help="Optional development-only retrieval gold set"
    )
    args = parser.parse_args()
    os.umask(0o077)
    args.sandbox.mkdir(parents=True, exist_ok=True)
    args.output_dir.mkdir(parents=True, exist_ok=True)
    if (
        shutil.disk_usage(args.sandbox).free < 50 * 1024**3
        or shutil.disk_usage("/").free < 5 * 1024**3
    ):
        raise RuntimeError("worker disk capacity gate failed")
    source, identity = pin(args.source, args.sandbox)
    baseline = args.sandbox / "awi-before"
    if not baseline.exists():
        shutil.copy2(args.baseline, baseline)
    save(
        args.output_dir / "corpus.json",
        {
            "review_status": "candidate_pending_dual_review",
            "split": "dev",
            "shards": identity,
            "baseline_sha256": hashlib.sha256(baseline.read_bytes()).hexdigest(),
            "candidate_sha256": hashlib.sha256(args.candidate.read_bytes()).hexdigest()
            if args.candidate
            else None,
        },
    )
    if args.pin_only:
        return
    if not args.candidate:
        parser.error("--candidate required without --pin-only")
    report = json.loads(args.observations.read_text())
    if report.get("split") == "frozen":
        raise ValueError("frozen evaluation is forbidden")
    calls = []
    seen = set()
    for row in report["observations"]:
        arguments = row["arguments"]
        key = json.dumps(arguments, sort_keys=True)
        if key not in seen:
            seen.add(key)
            calls.append(arguments)
    save(args.output_dir / "queries.json", calls)
    for repetition, order in enumerate([["before", "after"], ["after", "before"]]):
        for name in order:
            binary = baseline if name == "before" else args.candidate
            run_arm(
                binary,
                name,
                repetition,
                source,
                args.sandbox,
                calls,
                args.output_dir / f"{name}-{repetition}.json",
            )
    if args.gold:
        gold = json.loads(args.gold.read_text())
        if gold.get("split") == "frozen":
            raise ValueError("frozen evaluation is forbidden")
        for name, binary in [("before", baseline), ("after", args.candidate)]:
            print(f"Retrieval benchmark: {name}", flush=True)
            with (args.output_dir / f"benchmark-{name}.log").open("w") as log:
                subprocess.run(
                    [
                        str(binary),
                        "--index-dir",
                        str(args.sandbox / name / "snapshot-cache"),
                        "benchmark",
                        "--snapshot-source",
                        str(source),
                        "--gold",
                        str(args.gold),
                        "--allow-draft",
                        "--output",
                        str(args.output_dir / f"benchmark-{name}.json"),
                    ],
                    stdout=log,
                    stderr=log,
                    timeout=600,
                    check=True,
                )


if __name__ == "__main__":
    main()
