#!/usr/bin/env python3
"""Audit call delivery and observable follow-ups; never infer relevance/adoption."""

from __future__ import annotations

import argparse
import collections
import datetime as dt
import json
import math
import os
import re
from pathlib import Path

from task_outcomes import LABELS, apply_reviews, collect_tasks, summarize_tasks

AWI_TOOLS = {"workspace_search", "workspace_inspect", "workspace_query"}
DISCOVERY = re.compile(r"(?:^|[\s;|])(?:rg|fd|find|grep|rga)(?:\s|$)")
READ_ERRORS = collections.Counter()


def records(path):
    with path.open(encoding="utf-8") as handle:
        for line in handle:
            if line.strip():
                try:
                    yield json.loads(line)
                except json.JSONDecodeError:
                    READ_ERRORS[str(path)] += 1


def percentile(values, fraction):
    values = sorted(values)
    return values[max(0, math.ceil(len(values) * fraction) - 1)] if values else None


def distribution(rows):
    durations = [row["duration_ms"] for row in rows]
    return {
        "calls": len(rows),
        "status": dict(collections.Counter(row["status"] for row in rows)),
        "p50_ms": percentile(durations, 0.5),
        "p95_ms": percentile(durations, 0.95),
        "over_1s": sum(value > 1000 for value in durations),
        "over_5s": sum(value > 5000 for value in durations),
        "over_100kib": sum(row["response_bytes"] > 102400 for row in rows),
        "text_bytes_p50": percentile(
            [
                row["text_content_bytes"]
                for row in rows
                if row.get("text_content_bytes") is not None
            ],
            0.5,
        ),
        "structured_bytes_p50": percentile(
            [
                row["structured_content_bytes"]
                for row in rows
                if row.get("structured_content_bytes") is not None
            ],
            0.5,
        ),
    }


def audit_summary(rows):
    searches = [row for row in rows if row["tool"] == "workspace_search"]
    inspections = [row for row in rows if row["tool"] == "workspace_inspect"]
    by_client = collections.defaultdict(list)
    for row in rows:
        by_client[row.get("client_name", "unknown")].append(row)
    by_pid = collections.Counter(row["pid"] for row in rows)
    search_ids = {row["search_id"]: row for row in searches if row.get("search_id")}
    linked = [row for row in inspections if row.get("parent_search_id") in search_ids]
    recent_by_pid = {}
    near_simultaneous = 0
    for row in sorted(searches, key=lambda row: row["timestamp_unix_ms"]):
        previous = recent_by_pid.get(row["pid"])
        if previous and row["timestamp_unix_ms"] - previous["timestamp_unix_ms"] < 500:
            near_simultaneous += 1
        recent_by_pid[row["pid"]] = row
    return {
        **distribution(rows),
        "by_tool": {
            tool: distribution([row for row in rows if row["tool"] == tool])
            for tool in sorted(AWI_TOOLS)
        },
        "by_day": dict(
            sorted(
                collections.Counter(
                    dt.datetime.fromtimestamp(row["timestamp_unix_ms"] / 1000, dt.UTC)
                    .date()
                    .isoformat()
                    for row in rows
                ).items()
            )
        ),
        "by_client": {
            name: {
                **distribution(items),
                "sessions": len(
                    {row["session_id"] for row in items if row.get("session_id")}
                ),
            }
            for name, items in sorted(by_client.items())
        },
        "by_process": dict(
            collections.Counter(row.get("client_process", "unknown") for row in rows)
        ),
        "distinct_pids": len(by_pid),
        "busiest_pid_calls": by_pid.most_common(1),
        "synthetic_marker": dict(
            collections.Counter(
                str(row.get("synthetic", "unknown")).lower() for row in rows
            )
        ),
        "calls_with_session_id": sum(bool(row.get("session_id")) for row in rows),
        "successful_searches": sum(row["status"] == "ok" for row in searches),
        "zero_hit_searches": sum(row.get("result_count") == 0 for row in searches),
        "searches_with_context": sum(
            bool(row["arguments"].get("context_path")) for row in searches
        ),
        "memory_filtered_searches": sum(
            "agent_memory" in (row["arguments"].get("kinds") or []) for row in searches
        ),
        "requested_limits": dict(
            collections.Counter(str(row["arguments"].get("limit")) for row in searches)
        ),
        "result_counts": dict(
            collections.Counter(str(row.get("result_count")) for row in searches)
        ),
        "linked_inspections": len(linked),
        "near_simultaneous_same_pid_searches": near_simultaneous,
        "errors": [
            {
                key: row.get(key)
                for key in ("tool", "error_code", "error_message", "timestamp_unix_ms")
            }
            for row in rows
            if row["status"] != "ok"
        ],
        "limitations": [
            "synthetic=false is not proof of production traffic; historical smoke calls can be unmarked",
            "an MCP PID may multiplex sessions; PID sequences are not session-level behavior",
            "nonempty hits and inspected paths do not establish evidence adoption",
        ],
    }


def decode_output(output):
    if isinstance(output, str):
        text = output.split("Output:\n", 1)[-1].strip()
        try:
            value = json.loads(text)
        except json.JSONDecodeError:
            return {}, text
    else:
        value = output
    if isinstance(value, dict):
        if "structuredContent" in value:
            return value["structuredContent"], "\n".join(
                block.get("text", "") for block in value.get("content", [])
            )
        return value, json.dumps(value, ensure_ascii=False)
    if isinstance(value, list):
        text = "\n".join(
            block.get("text", "") for block in value if isinstance(block, dict)
        )
        try:
            return json.loads(text), text
        except json.JSONDecodeError:
            return {}, text
    return {}, ""


def session_observations(path, project_root, rows=None):
    session = None
    pending = {}
    calls = []
    actions = []
    for line_number, row in enumerate(records(path) if rows is None else rows, 1):
        payload = row.get("payload", {})
        if row.get("type") == "session_meta":
            cwd = Path(payload.get("cwd", "/"))
            if not cwd.is_relative_to(project_root):
                return []
            session = payload.get("id")
        if row.get("type") != "response_item" or not session:
            continue
        if payload.get("type") == "function_call":
            try:
                args = json.loads(payload.get("arguments", "{}"))
            except json.JSONDecodeError:
                args = {}
            tool = payload.get("name")
            namespace = payload.get("namespace")
            awi = namespace == "mcp__awi" or str(tool).startswith("mcp__awi")
            action = {
                "tool": tool,
                "awi": awi,
                "arguments": args,
                "line": line_number,
            }
            actions.append(action)
            if awi and tool.split("__")[-1] in AWI_TOOLS:
                call = {
                    "session_id": session,
                    "source": str(path),
                    "call_id": payload["call_id"],
                    "action_index": len(actions) - 1,
                    "timestamp": row.get("timestamp"),
                    **action,
                }
                calls.append(call)
                pending[payload["call_id"]] = call
        elif payload.get("type") == "function_call_output":
            call = pending.get(payload.get("call_id"))
            if call is not None:
                call["result"], call["visible_text"] = decode_output(
                    payload.get("output")
                )
                call["output_line"] = line_number
    observations = []
    for call in calls:
        if not call["tool"].endswith("workspace_search"):
            continue
        result = call.get("result", {})
        hits = result.get("hits", [])
        paths = [hit.get("path", "") for hit in hits]
        text = call.get("visible_text", "")
        follows = []
        for action in actions[call["action_index"] + 1 :]:
            if action["line"] <= call.get("output_line", 0):
                continue
            if action["awi"] and action["tool"].endswith("workspace_search"):
                break
            follows.append(action)
            if len(follows) == 3:
                break
        follow_text = json.dumps(follows, ensure_ascii=False)
        observations.append(
            {
                key: value
                for key, value in {
                    **call,
                    "result": None,
                    "visible_text": None,
                    "top_paths": paths,
                    "result_available": "result" in call,
                    "result_count": len(hits) if "hits" in result else None,
                    "error": result.get("error"),
                    "text_only_path_summary": text.startswith("AWI search returned")
                    and "Top paths:" in text,
                    "structured_previews_observed": any(
                        hit.get("preview") for hit in hits
                    ),
                    "followup": follows,
                    "returned_path_in_followup": any(
                        path and path in follow_text for path in paths
                    ),
                    "discovery_in_followup": any(
                        DISCOVERY.search(str(action["arguments"].get("cmd", "")))
                        for action in follows
                    ),
                    "evidence_adopted": None,
                }.items()
                if key not in {"result", "visible_text", "action_index"}
            }
        )
    return observations


def write_private(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    os.fchmod(fd, 0o600)
    with os.fdopen(fd, "w") as handle:
        json.dump(value, handle, ensure_ascii=False, indent=2)
        handle.write("\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--audit", type=Path, action="append", required=True)
    parser.add_argument("--codex-sessions", type=Path, action="append", default=[])
    parser.add_argument("--project-root", type=Path, required=True)
    parser.add_argument("--since", default="1970-01-01")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--reviews", type=Path, action="append", default=[])
    parser.add_argument("--review-template", type=Path)
    args = parser.parse_args()
    since = dt.datetime.fromisoformat(args.since).replace(tzinfo=dt.UTC)
    rows = [
        row
        for path in args.audit
        for row in records(path)
        if row["timestamp_unix_ms"] >= since.timestamp() * 1000
    ]
    observations = []
    tasks = []
    visited = set()
    for directory in args.codex_sessions:
        for path in sorted(directory.rglob("*.jsonl")):
            if path.resolve() in visited:
                continue
            visited.add(path.resolve())
            session_rows = list(records(path))
            observations.extend(
                session_observations(path, args.project_root, session_rows)
            )
            tasks.extend(
                collect_tasks(session_rows, path, args.project_root, decode_output)
            )
    observations = [
        item
        for item in observations
        if (item.get("timestamp") or "")[:10] >= args.since
    ]
    tasks = [
        task for task in tasks if (task.get("start_timestamp") or "")[:10] >= args.since
    ]
    apply_reviews(tasks, [json.loads(path.read_text()) for path in args.reviews])
    by_call = {
        (task["session_id"], search["call_id"]): (task, search)
        for task in tasks
        for search in task["searches"]
    }
    for observation in observations:
        linked = by_call.get((observation["session_id"], observation["call_id"]))
        if linked:
            task, search = linked
            observation.update(
                task_id=task["task_id"],
                search_id=search["search_id"],
                search_key=search["key"],
            )
    if args.review_template:
        write_private(
            args.review_template,
            {
                "split": "dev",
                "reviews": [
                    {
                        "task_id": task["task_id"],
                        "reviewer_id": "",
                        "independent": False,
                        **dict.fromkeys(LABELS),
                        "adopted_search_keys": [],
                        "supporting_evidence": "",
                    }
                    for task in tasks
                ],
            },
        )
    report = {
        "schema_version": 2,
        "split": "dev",
        "review_status": "candidate_pending_dual_review",
        "malformed_records_by_source": dict(READ_ERRORS),
        "audit": audit_summary(rows),
        "trace_diagnostics": {
            "searches": len(observations),
            "task_linked_searches": sum(
                bool(item.get("task_id")) for item in observations
            ),
            "task_unlinked_searches": sum(
                not item.get("task_id") for item in observations
            ),
            "sessions": len({item["session_id"] for item in observations}),
            **{
                key: sum(bool(item[key]) for item in observations)
                for key in (
                    "result_available",
                    "text_only_path_summary",
                    "structured_previews_observed",
                    "returned_path_in_followup",
                    "discovery_in_followup",
                )
            },
            "evidence_adopted": "unknown; follow-up flags require relevance review",
            "sampling": "first three tool calls after result, stopping before next AWI search",
        },
        "task_diagnostics": summarize_tasks(tasks),
        "tasks": tasks,
        "observations": observations,
    }
    write_private(args.output, report)
    print(
        json.dumps(
            {
                key: value
                for key, value in report.items()
                if key not in {"observations", "tasks"}
            },
            ensure_ascii=False,
            indent=2,
        )
    )


if __name__ == "__main__":
    main()
