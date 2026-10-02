#!/usr/bin/env python3
"""Private task packets and conservative, independently reviewed adoption metrics."""

from __future__ import annotations

import collections
import datetime as dt
import json
import math
import re
from pathlib import Path

LABELS = (
    "retrieval_eligible",
    "answer_correct",
    "evidence_correct",
    "evidence_adopted",
)
DISCOVERY = re.compile(r"(?:^|[\s;|])(?:rg|fd|find|grep|rga)(?:\s|$)")


def message_text(payload):
    return "\n".join(
        block.get("text", "")
        for block in payload.get("content", [])
        if isinstance(block, dict)
    )


def elapsed(start, end):
    try:
        return max(
            0,
            (
                dt.datetime.fromisoformat(end) - dt.datetime.fromisoformat(start)
            ).total_seconds(),
        )
    except (ValueError, TypeError):
        return None


def collect_tasks(rows, source, project_root, decode_output):
    """Use actual user response items as observable turn boundaries, never PID."""
    session, current = None, None
    tasks, pending = [], {}
    for line, row in enumerate(rows, 1):
        payload = row.get("payload", {})
        if row.get("type") == "session_meta":
            if not Path(payload.get("cwd", "/")).is_relative_to(project_root):
                return []
            session = payload.get("id")
        if not session or row.get("type") != "response_item":
            continue
        kind = payload.get("type")
        if kind == "message" and payload.get("role") == "user":
            current = {
                "task_id": f"{session}:user-line-{line}",
                "session_id": session,
                "source": str(source),
                "start_line": line,
                "start_timestamp": row.get("timestamp"),
                "question": message_text(payload)[:20000],
                "actions": [],
                "searches": [],
                "final": None,
                "tool_visible": None,
                "namespace_loaded": None,
                "boundary": "user_response_item",
            }
            tasks.append(current)
        elif current and kind == "function_call":
            try:
                arguments = json.loads(payload.get("arguments", "{}"))
            except (TypeError, json.JSONDecodeError):
                arguments = {}
            if not isinstance(arguments, dict):
                arguments = {}
            name = str(payload.get("name", ""))
            awi = payload.get("namespace") == "mcp__awi" or name.startswith("mcp__awi")
            current["actions"].append(
                {"tool": name, "awi": awi, "arguments": arguments, "line": line}
            )
            if awi:
                current["namespace_loaded"] = current["tool_visible"] = True
            if awi and name.endswith("workspace_search"):
                search = {
                    "key": f"{session}:{payload.get('call_id')}",
                    "call_id": payload.get("call_id"),
                    "line": line,
                    "arguments": arguments,
                    "search_id": None,
                    "hits": [],
                    "output_observed": False,
                }
                current["searches"].append(search)
                pending[payload.get("call_id")] = search
        elif kind == "function_call_output":
            search = pending.get(payload.get("call_id"))
            if search is not None:
                result, text = decode_output(payload.get("output"))
                if not isinstance(result, dict):
                    result = {}
                search.update(
                    search_id=result.get("search_id"),
                    output_observed=True,
                    output_line=line,
                    error=result.get("error"),
                    hits=[
                        {
                            key: hit.get(key)
                            for key in ("path", "generation", "preview", "symbol")
                        }
                        for hit in result.get("hits", [])
                        if isinstance(hit, dict)
                    ],
                    visible_text_chars=len(text),
                )
        elif current and kind == "message" and payload.get("role") == "assistant":
            if (
                payload.get("phase") == "final_answer"
                or payload.get("channel") == "final"
            ):
                text = message_text(payload)
                current["final"] = {
                    "text": text[:20000],
                    "chars": len(text),
                    "line": line,
                }
                current["end_timestamp"] = row.get("timestamp")
    for index, task in enumerate(tasks):
        task["end_line"] = (
            tasks[index + 1]["start_line"] - 1 if index + 1 < len(tasks) else None
        )
        final = task["final"]
        final_text = final["text"] if final else ""
        actions = [a for a in task["actions"] if not final or a["line"] < final["line"]]
        task["actions"] = actions
        task["searches"] = [
            s for s in task["searches"] if not final or s["line"] < final["line"]
        ]
        queries = [json.dumps(s["arguments"], sort_keys=True) for s in task["searches"]]
        for search in task["searches"]:
            paths = [h["path"] for h in search["hits"] if h.get("path")]
            follows = [
                a for a in actions if a["line"] > search.get("output_line", 10**12)
            ]
            search["followup_action_count"] = len(follows)
            search["path_reuse_candidates"] = [
                path
                for path in paths
                if any(path in json.dumps(a, ensure_ascii=False) for a in follows)
            ]
            search["final_path_candidates"] = [
                path for path in paths if path in final_text
            ]
        task["metrics"] = {
            "agent_tool_calls": len(actions),
            "awi_searches": len(task["searches"]),
            "awi_inspections": sum(
                a["awi"] and a["tool"].endswith("workspace_inspect") for a in actions
            ),
            "shell_discoveries": sum(
                bool(DISCOVERY.search(str(a["arguments"].get("cmd", ""))))
                for a in actions
                if not a["awi"]
            ),
            "repeated_exact_searches": len(queries) - len(set(queries)),
            "observed_turn_wall_seconds": elapsed(
                task["start_timestamp"], task.get("end_timestamp")
            ),
        }
        task["review"] = {"status": "unreviewed", **dict.fromkeys(LABELS)}
    return tasks


def apply_reviews(tasks, documents):
    by_id = {task["task_id"]: task for task in tasks}
    grouped, seen = collections.defaultdict(list), set()
    for document in documents:
        if document.get("split") != "dev":
            raise ValueError(
                "review input must explicitly use split=dev; frozen is forbidden"
            )
        for row in document.get("reviews", []):
            task_id, reviewer = row.get("task_id"), row.get("reviewer_id")
            if task_id not in by_id:
                raise ValueError(f"unmatched task review: {task_id}")
            if not isinstance(reviewer, str) or not reviewer.strip():
                raise ValueError("reviewer_id is required")
            key = (task_id, reviewer.strip().casefold())
            if key in seen:
                raise ValueError("duplicate task/reviewer review")
            seen.add(key)
            evidence = row.get("supporting_evidence")
            if (
                row.get("independent") is not True
                or not isinstance(evidence, str)
                or not evidence.strip()
            ):
                raise ValueError(
                    "review needs independence attestation and supporting evidence"
                )
            if any(type(row.get(label)) not in (bool, type(None)) for label in LABELS):
                raise ValueError("review labels must be boolean or null")
            cited = row.get("adopted_search_keys", [])
            valid = {search["key"] for search in by_id[task_id]["searches"]}
            if not isinstance(cited, list) or any(
                not isinstance(key, str) or key not in valid for key in cited
            ):
                raise ValueError(
                    "adopted_search_keys must reference searches in this task"
                )
            if row.get("evidence_adopted") is True and not cited:
                raise ValueError("adoption requires at least one attributable search")
            if row.get("evidence_adopted") is False and cited:
                raise ValueError("non-adoption cannot cite adopted searches")
            grouped[task_id].append(row)
    for task in tasks:
        reviews = grouped[task["task_id"]]
        values = [
            tuple(row.get(label) for label in LABELS)
            + (tuple(sorted(row.get("adopted_search_keys", []))),)
            for row in reviews
        ]
        status = "unreviewed"
        if reviews:
            status = (
                "single_review"
                if len(reviews) == 1
                else ("agreed" if len(set(values)) == 1 else "disputed")
            )
        agreed = status == "agreed"
        task["review"] = {
            "status": status,
            "reviewer_ids": [row["reviewer_id"] for row in reviews],
            **{label: reviews[0].get(label) if agreed else None for label in LABELS},
            "adopted_search_keys": reviews[0].get("adopted_search_keys", [])
            if agreed
            else None,
            "annotations": reviews,
        }


def summarize_tasks(tasks):
    eligible = [t for t in tasks if t["review"]["retrieval_eligible"] is True]
    known = [
        t
        for t in eligible
        if t["review"]["evidence_adopted"] is False
        or (
            t["review"]["evidence_adopted"] is True
            and t["review"]["evidence_correct"] is not None
        )
    ]
    adopted = [
        t
        for t in known
        if t["review"]["evidence_adopted"] is True
        and t["review"]["evidence_correct"] is True
    ]
    walls = sorted(
        t["metrics"]["observed_turn_wall_seconds"]
        for t in tasks
        if t["metrics"]["observed_turn_wall_seconds"] is not None
    )
    answers = [t for t in eligible if t["review"]["answer_correct"] is not None]
    return {
        "client": "codex_response_item_traces",
        "tasks_observed": len(tasks),
        "tasks_with_final": sum(t["final"] is not None for t in tasks),
        "review_status": dict(
            collections.Counter(t["review"]["status"] for t in tasks)
        ),
        "eligible_reviewed_tasks": len(eligible),
        "eligible_with_known_adoption_outcome": len(known),
        "answer_reviewed_tasks": len(answers),
        "answer_accuracy_on_reviewed": sum(
            t["review"]["answer_correct"] for t in answers
        )
        / len(answers)
        if answers
        else None,
        "observed_turn_wall_seconds": {
            "known": len(walls),
            "unknown": len(tasks) - len(walls),
            "total": sum(walls),
            "mean": sum(walls) / len(walls) if walls else None,
            "p50": walls[math.ceil(len(walls) * 0.5) - 1] if walls else None,
            "p95": walls[math.ceil(len(walls) * 0.95) - 1] if walls else None,
        },
        "correct_evidence_adopted": len(adopted),
        "correct_adoption_rate_on_known": len(adopted) / len(known) if known else None,
        "adoption_review_coverage": len(known) / len(eligible) if eligible else None,
        "correct_adoption_bounds_on_eligible": [
            len(adopted) / len(eligible),
            (len(adopted) + len(eligible) - len(known)) / len(eligible),
        ]
        if eligible
        else None,
        "funnel_on_reviewed_eligible": {
            "tool_visible": sum(t["tool_visible"] is True for t in eligible),
            "tool_visibility_unknown": sum(t["tool_visible"] is None for t in eligible),
            "namespace_loaded": sum(t["namespace_loaded"] is True for t in eligible),
            "namespace_unknown": sum(t["namespace_loaded"] is None for t in eligible),
            "search": sum(t["metrics"]["awi_searches"] > 0 for t in eligible),
            "inspect": sum(t["metrics"]["awi_inspections"] > 0 for t in eligible),
            "correct_evidence_adopted": len(adopted),
            "adopted_without_inspect": sum(
                t["metrics"]["awi_inspections"] == 0 for t in adopted
            ),
        },
        "observed_metrics": {
            key: sum(t["metrics"][key] for t in tasks)
            for key in (
                "agent_tool_calls",
                "awi_searches",
                "awi_inspections",
                "shell_discoveries",
                "repeated_exact_searches",
            )
        },
        "limitations": [
            "User response-item turns are observable packets, not guaranteed complete multi-turn tasks.",
            "Finals require explicit final_answer phase or final channel; absence remains unknown.",
            "Exact path reuse and citation candidates are review aids, never correctness labels.",
            "Independence is attested by reviewers, not verified by this script.",
            "Tool visibility and namespace loading without an AWI call remain unknown.",
            "Turn wall time includes non-retrieval work; these observations are not causal A/B evidence.",
        ],
    }
