#!/usr/bin/env python3
"""Run a controlled Codex baseline-vs-AWI retrieval experiment."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import random
import re
import shutil
import signal
import statistics
import subprocess
import time
from collections import defaultdict
from pathlib import Path
from typing import Any

DEFAULT_MODEL = "llmbox-gpt-5.6-sol"
DEFAULT_OUTPUT_ROOT = Path(
    "/mnt/bn/baiweikang/qianchuan_distill/awi/agent_eval/"
    "two_shot_plus_tongyong_agent_ab_20260920"
)
MIN_TMP_FREE_BYTES = 50 * 1024**3
MIN_SYSTEM_FREE_BYTES = 5 * 1024**3
SEARCH_COMMAND = re.compile(r"(?<![\w-])(rg|fd|grep|find)(?![\w-])")
INSPECT_COMMAND = re.compile(r"(?<![\w-])(sed|head|tail|cat|jq|awk)(?![\w-])")


def parse_args() -> argparse.Namespace:
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser()
    parser.add_argument("--tasks", type=Path, default=here / "tasks.dev.json")
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUTPUT_ROOT)
    parser.add_argument("--schema", type=Path, default=here / "output_schema.json")
    parser.add_argument(
        "--codex-bin", type=Path, default=Path("/home/tiger/.local/bin/codex")
    )
    parser.add_argument(
        "--awi-bin", type=Path, default=Path("/tmp/awi-target/release/awi")
    )
    parser.add_argument(
        "--awi-index",
        type=Path,
        default=Path("/tmp/awi-qianchuan-candidate-v2-index"),
    )
    parser.add_argument(
        "--awi-socket",
        type=Path,
        default=Path("/tmp/two_shot_plus_tongyong_agent_ab_20260920/awi.sock"),
    )
    parser.add_argument(
        "--sandbox-root",
        type=Path,
        default=Path("/tmp/two_shot_plus_tongyong_agent_ab_20260920"),
    )
    parser.add_argument("--model", default=DEFAULT_MODEL)
    parser.add_argument("--repetitions", type=int, default=2)
    parser.add_argument("--limit-tasks", type=int)
    parser.add_argument("--task-id", action="append")
    parser.add_argument("--timeout-seconds", type=int, default=240)
    parser.add_argument("--seed", type=int, default=20260920)
    parser.add_argument("--resume", action="store_true")
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    validate_args(args)
    tasks_document = json.loads(args.tasks.read_text())
    tasks = tasks_document["tasks"]
    if args.task_id:
        requested = set(args.task_id)
        tasks = [task for task in tasks if task["id"] in requested]
        found = {task["id"] for task in tasks}
        if missing := sorted(requested - found):
            raise SystemExit(f"unknown task IDs: {', '.join(missing)}")
    if args.limit_tasks is not None:
        tasks = tasks[: args.limit_tasks]
    tasks_document = {**tasks_document, "tasks": tasks}

    prepare_sandbox(args)
    results_path = args.output_dir / "results.jsonl"
    if results_path.exists() and not args.resume:
        raise SystemExit(
            f"results already exist; use --resume or a new output directory: {results_path}"
        )
    args.output_dir.mkdir(parents=True, exist_ok=True)
    existing = load_existing(results_path) if args.resume else {}
    run_plan = build_run_plan(tasks, args.repetitions, args.seed)

    results = list(existing.values())
    for run_index, (repetition, task, arm) in enumerate(run_plan, 1):
        key = result_key(task["id"], repetition, arm)
        if key in existing:
            continue
        print(
            f"[{run_index}/{len(run_plan)}] repetition={repetition} "
            f"task={task['id']} arm={arm}",
            flush=True,
        )
        result = run_trial(args, tasks_document, task, repetition, arm, run_index)
        append_jsonl(args.output_dir / "results.jsonl", result)
        results.append(result)
        write_json(
            args.output_dir / "summary.json", summarize(results, args, tasks_document)
        )

    summary = summarize(results, args, tasks_document)
    write_json(args.output_dir / "summary.json", summary)
    print(json.dumps(summary, ensure_ascii=False, indent=2))


def validate_args(args: argparse.Namespace) -> None:
    if args.repetitions < 1:
        raise SystemExit("--repetitions must be positive")
    if args.limit_tasks is not None and args.limit_tasks < 1:
        raise SystemExit("--limit-tasks must be positive")
    for path in (args.tasks, args.schema, args.codex_bin, args.awi_bin):
        if not path.exists():
            raise SystemExit(f"required path does not exist: {path}")
    if not args.awi_index.joinpath("catalog.sqlite3").is_file():
        raise SystemExit(f"AWI index is missing catalog.sqlite3: {args.awi_index}")
    if not args.sandbox_root.resolve().is_relative_to("/tmp"):
        raise SystemExit(
            f"sandbox root must be under worker-local /tmp: {args.sandbox_root}"
        )
    tmp_free = shutil.disk_usage("/tmp").free
    if tmp_free < MIN_TMP_FREE_BYTES:
        raise SystemExit(
            f"worker /tmp has only {tmp_free / 1024**3:.1f} GiB free; "
            f"at least {MIN_TMP_FREE_BYTES / 1024**3:.0f} GiB is required"
        )
    system_free = shutil.disk_usage("/").free
    if system_free < MIN_SYSTEM_FREE_BYTES:
        raise SystemExit(
            f"system disk has only {system_free / 1024**3:.1f} GiB free; "
            f"at least {MIN_SYSTEM_FREE_BYTES / 1024**3:.0f} GiB is required"
        )
    daemon = subprocess.run(
        [
            str(args.awi_bin),
            "--index-dir",
            str(args.awi_index),
            "--socket",
            str(args.awi_socket),
            "ping",
            "--json",
        ],
        check=False,
        capture_output=True,
        text=True,
    )
    if daemon.returncode != 0:
        raise SystemExit(f"AWI daemon preflight failed: {daemon.stderr.strip()}")


def prepare_sandbox(args: argparse.Namespace) -> None:
    args.sandbox_root.mkdir(parents=True, exist_ok=True)
    for arm in ("baseline", "awi"):
        home = args.sandbox_root / f"codex-home-{arm}"
        (home / "tmp").mkdir(parents=True, exist_ok=True)
        (args.sandbox_root / f"cwd-{arm}").mkdir(parents=True, exist_ok=True)
        home.chmod(0o700)
        config = ""
        if arm == "awi":
            config = (
                "[mcp_servers.awi]\n"
                f'command = "{toml_string(args.awi_bin)}"\n'
                "args = ["
                + ", ".join(
                    f'"{toml_string(value)}"'
                    for value in (
                        "--index-dir",
                        args.awi_index,
                        "--socket",
                        args.awi_socket,
                        "mcp",
                    )
                )
                + "]\n"
                "startup_timeout_sec = 20\n"
                "tool_timeout_sec = 30\n"
            )
        (home / "config.toml").write_text(config)


def build_run_plan(
    tasks: list[dict[str, Any]], repetitions: int, seed: int
) -> list[tuple[int, dict[str, Any], str]]:
    plan: list[tuple[int, dict[str, Any], str]] = []
    for repetition in range(1, repetitions + 1):
        ordered = tasks.copy()
        random.Random(seed + repetition).shuffle(ordered)
        for task_index, task in enumerate(ordered):
            arms = ["baseline", "awi"]
            if (task_index + repetition) % 2:
                arms.reverse()
            plan.extend((repetition, task, arm) for arm in arms)
    return plan


def run_trial(
    args: argparse.Namespace,
    tasks_document: dict[str, Any],
    task: dict[str, Any],
    repetition: int,
    arm: str,
    run_index: int,
) -> dict[str, Any]:
    run_id = f"r{repetition:02d}-{task['id']}-{arm}"
    local_run = args.sandbox_root / "runs" / run_id
    local_run.mkdir(parents=True, exist_ok=True)
    final_path = local_run / "final.json"
    final_path.unlink(missing_ok=True)
    prompt = build_prompt(tasks_document["corpus_roots"], task["question"], arm)
    command = codex_command(args, arm, final_path)
    environment = os.environ.copy()
    environment.update(
        {
            "CODEX_HOME": str(args.sandbox_root / f"codex-home-{arm}"),
            "TMPDIR": str(args.sandbox_root / f"codex-home-{arm}" / "tmp"),
            "RUST_BACKTRACE": "0",
        }
    )

    started = time.monotonic()
    process = subprocess.Popen(
        command,
        cwd=args.sandbox_root / f"cwd-{arm}",
        env=environment,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    timed_out = False
    try:
        stdout, stderr = process.communicate(prompt, timeout=args.timeout_seconds)
    except subprocess.TimeoutExpired:
        timed_out = True
        os.killpg(process.pid, signal.SIGTERM)
        try:
            stdout, stderr = process.communicate(timeout=10)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            stdout, stderr = process.communicate()
    wall_seconds = time.monotonic() - started

    events = parse_events(stdout)
    final = load_final(final_path, events)
    metrics = tool_metrics(events)
    path_correct, answer_correct = score_answer(task, final)
    policy_violation = (
        metrics["shell_tool_calls"] > 0
        if arm == "awi"
        else metrics["awi_mcp_calls"] > 0 or metrics["awi_cli_calls"] > 0
    )
    usage = next(
        (
            event.get("usage", {})
            for event in reversed(events)
            if event.get("type") == "turn.completed"
        ),
        {},
    )
    result = {
        "run_index": run_index,
        "run_id": run_id,
        "repetition": repetition,
        "task_id": task["id"],
        "category": task["category"],
        "arm": arm,
        "model": args.model,
        "wall_seconds": wall_seconds,
        "timed_out": timed_out,
        "exit_code": process.returncode,
        "path_correct": path_correct,
        "answer_correct": answer_correct,
        "correct": bool(
            process.returncode == 0
            and not timed_out
            and not policy_violation
            and path_correct
            and answer_correct
        ),
        "policy_violation": policy_violation,
        "agent_tool_calls": metrics["agent_tool_calls"],
        "search_calls": metrics["search_calls"],
        "inspection_calls": metrics["inspection_calls"],
        "shell_tool_calls": metrics["shell_tool_calls"],
        "mcp_tool_calls": metrics["mcp_tool_calls"],
        "awi_mcp_calls": metrics["awi_mcp_calls"],
        "awi_cli_calls": metrics["awi_cli_calls"],
        "mcp_tools": metrics["mcp_tools"],
        "shell_commands": metrics["shell_commands"],
        "usage": usage,
        "answer": final,
        "stderr_tail": stderr[-2_000:],
    }
    (local_run / "stdout.jsonl").write_text(stdout)
    (local_run / "stderr.log").write_text(stderr)
    write_json(local_run / "result.json", result)

    raw_dir = args.output_dir / "raw"
    raw_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(local_run / "stdout.jsonl", raw_dir / f"{run_id}.jsonl")
    shutil.copy2(local_run / "stderr.log", raw_dir / f"{run_id}.stderr.log")
    shutil.copy2(local_run / "result.json", raw_dir / f"{run_id}.result.json")
    return result


def build_prompt(roots: list[str], question: str, arm: str) -> str:
    roots_text = "\n".join(f"- {root}" for root in roots)
    if arm == "awi":
        retrieval_rules = (
            "Use only the AWI MCP tools workspace_search, workspace_inspect, and "
            "workspace_query for discovery and evidence retrieval. Do not use shell "
            "commands for searching, listing, or reading corpus files. Pass allowed "
            "roots through workspace_search.roots. Use kinds/path_prefix only when the "
            "classification is known; omit kinds when uncertain (JSONL and TSV are "
            "tabular, while JSON is semi_structured). Do not put paths or filter syntax "
            "into the query text. Treat non-empty previews as direct evidence. Once an "
            "authoritative path is selected, do not search again: use one inspect (up to "
            "500 lines) for missing text details or workspace_query for structured "
            "aggregation. Keep workspace_search limit at 10 or less."
        )
    else:
        retrieval_rules = (
            "Use ordinary read-only shell retrieval commands such as rg, fd, sed, "
            "head, jq, and awk. Do not invoke AWI, its CLI, or any MCP tool."
        )
    return f"""You are in a controlled read-only retrieval benchmark.

Allowed corpus roots:
{roots_text}

Task:
{question}

Rules:
- {retrieval_rules}
- Search only the allowed corpus roots.
- Do not inspect any AWI evaluation, benchmark, task, report, or gold files.
- Do not use the network and do not modify any file.
- Stop as soon as you have enough direct evidence.
- Preserve exact identifier spellings and numeric values requested by the task; do not replace them with paraphrases.
- Put the concise factual result in `answer`.
- Put only the evidence file paths you actually used in `evidence_paths`.
"""


def codex_command(args: argparse.Namespace, arm: str, final_path: Path) -> list[str]:
    provider = (
        'model_providers.local-router={name="local-router", '
        'base_url="http://127.0.0.1:15800/v1", wire_api="responses", '
        'env_key="CODEX_ROUTER_API_KEY"}'
    )
    return [
        str(args.codex_bin),
        "exec",
        "--ephemeral",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--model",
        args.model,
        "--config",
        'model_provider="local-router"',
        "--config",
        'model_catalog_json="/home/tiger/.codex/model-catalogs/multi-provider-catalog.json"',
        "--config",
        provider,
        "--config",
        'model_reasoning_effort="medium"',
        "--config",
        'model_reasoning_summary="none"',
        "--config",
        "features.memories=false",
        "--config",
        "features.multi_agent=false",
        "--enable",
        "skip_host_skill_discovery",
        "--config",
        "suppress_unstable_features_warning=true",
        "--output-schema",
        str(args.schema),
        "--output-last-message",
        str(final_path),
        "--color",
        "never",
        "--json",
        "-C",
        str(args.sandbox_root / f"cwd-{arm}"),
        "-",
    ]


def parse_events(stdout: str) -> list[dict[str, Any]]:
    events = []
    for line in stdout.splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(event, dict):
            events.append(event)
    return events


def load_final(path: Path, events: list[dict[str, Any]]) -> dict[str, Any] | None:
    candidates = []
    if path.is_file():
        candidates.append(path.read_text())
    candidates.extend(
        item.get("text", "")
        for event in reversed(events)
        if event.get("type") == "item.completed"
        and (item := event.get("item", {})).get("type") == "agent_message"
    )
    for candidate in candidates:
        try:
            value = json.loads(candidate)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict):
            return value
    return None


def tool_metrics(events: list[dict[str, Any]]) -> dict[str, Any]:
    shell_commands = []
    mcp_tools = []
    for event in events:
        if event.get("type") != "item.completed":
            continue
        item = event.get("item", {})
        if item.get("type") == "command_execution":
            shell_commands.append(str(item.get("command", "")))
        elif item.get("type") == "mcp_tool_call":
            mcp_tools.append(
                {
                    "server": item.get("server"),
                    "tool": item.get("tool"),
                    "status": item.get("status"),
                }
            )

    awi_mcp_calls = sum(
        call["server"] == "awi"
        and call["tool"] in {"workspace_search", "workspace_inspect", "workspace_query"}
        for call in mcp_tools
    )
    awi_cli_calls = sum(
        bool(re.search(r"(?<![\w-])awi\s+.*\b(search|inspect|query)\b", command))
        for command in shell_commands
    )
    search_calls = sum(
        bool(SEARCH_COMMAND.search(command)) for command in shell_commands
    )
    search_calls += sum(call["tool"] == "workspace_search" for call in mcp_tools)
    inspection_calls = sum(
        bool(INSPECT_COMMAND.search(command)) for command in shell_commands
    )
    inspection_calls += sum(
        call["tool"] in {"workspace_inspect", "workspace_query"} for call in mcp_tools
    )
    return {
        "agent_tool_calls": len(shell_commands) + len(mcp_tools),
        "search_calls": search_calls,
        "inspection_calls": inspection_calls,
        "shell_tool_calls": len(shell_commands),
        "mcp_tool_calls": len(mcp_tools),
        "awi_mcp_calls": awi_mcp_calls,
        "awi_cli_calls": awi_cli_calls,
        "mcp_tools": mcp_tools,
        "shell_commands": shell_commands,
    }


def score_answer(
    task: dict[str, Any], final: dict[str, Any] | None
) -> tuple[bool, bool]:
    if final is None:
        return False, False
    evidence_paths = final.get("evidence_paths")
    answer = final.get("answer")
    if not isinstance(evidence_paths, list) or not isinstance(answer, str):
        return False, False
    path_correct = all(
        any(str(path).endswith(expected) for path in evidence_paths)
        for expected in task["expected_paths"]
    )
    normalized_answer = normalize(answer)
    answer_correct = all(
        normalize(token) in normalized_answer
        for token in task["required_answer_tokens"]
    )
    return path_correct, answer_correct


def normalize(value: str) -> str:
    return "".join(
        character.casefold()
        for character in value
        if character.isalnum() or character == "_"
    )


def summarize(
    results: list[dict[str, Any]],
    args: argparse.Namespace,
    tasks_document: dict[str, Any],
) -> dict[str, Any]:
    by_arm = {}
    for arm in ("baseline", "awi"):
        arm_results = [result for result in results if result["arm"] == arm]
        by_arm[arm] = aggregate(arm_results)

    pairs = defaultdict(dict)
    for result in results:
        pairs[(result["repetition"], result["task_id"])][result["arm"]] = result
    complete_pairs = [
        pair for pair in pairs.values() if "baseline" in pair and "awi" in pair
    ]
    both_correct = [
        pair
        for pair in complete_pairs
        if pair["baseline"]["correct"] and pair["awi"]["correct"]
    ]
    tool_deltas = [
        pair["baseline"]["agent_tool_calls"] - pair["awi"]["agent_tool_calls"]
        for pair in both_correct
    ]
    wall_deltas = [
        pair["baseline"]["wall_seconds"] - pair["awi"]["wall_seconds"]
        for pair in both_correct
    ]
    search_deltas = [
        pair["baseline"]["search_calls"] - pair["awi"]["search_calls"]
        for pair in both_correct
    ]
    return {
        "experiment": tasks_document["name"],
        "asset_family": tasks_document["asset_family"],
        "split": tasks_document["split"],
        "review_status": tasks_document["review_status"],
        "model": args.model,
        "repetitions": args.repetitions,
        "task_count": len(tasks_document["tasks"])
        if args.limit_tasks is None
        else min(args.limit_tasks, len(tasks_document["tasks"])),
        "completed_runs": len(results),
        "arms": by_arm,
        "paired": {
            "complete_pairs": len(complete_pairs),
            "both_correct_pairs": len(both_correct),
            "agent_tool_call_reduction": reduction(
                [pair["baseline"]["agent_tool_calls"] for pair in both_correct],
                [pair["awi"]["agent_tool_calls"] for pair in both_correct],
            ),
            "search_call_reduction": reduction(
                [pair["baseline"]["search_calls"] for pair in both_correct],
                [pair["awi"]["search_calls"] for pair in both_correct],
            ),
            "wall_time_reduction": reduction(
                [pair["baseline"]["wall_seconds"] for pair in both_correct],
                [pair["awi"]["wall_seconds"] for pair in both_correct],
            ),
            "median_agent_tool_call_delta": median(tool_deltas),
            "median_search_call_delta": median(search_deltas),
            "median_wall_seconds_delta": median(wall_deltas),
            "awi_wall_time_wins": sum(delta > 0 for delta in wall_deltas),
            "baseline_wall_time_wins": sum(delta < 0 for delta in wall_deltas),
            "ties": sum(delta == 0 for delta in wall_deltas),
            "wall_delta_mean_95pct_bootstrap_ci": bootstrap_mean_ci(
                wall_deltas, args.seed
            ),
            "tool_call_delta_mean_95pct_bootstrap_ci": bootstrap_mean_ci(
                tool_deltas, args.seed + 1
            ),
        },
        "disk": {
            "sandbox_root": str(args.sandbox_root),
            "output_dir": str(args.output_dir),
            "tmp_free_bytes": shutil.disk_usage("/tmp").free,
            "system_free_bytes": shutil.disk_usage("/").free,
            "sandbox_bytes": directory_size(args.sandbox_root),
        },
        "tasks_sha256": sha256_file(args.tasks),
        "generated_at_unix_seconds": time.time(),
    }


def aggregate(results: list[dict[str, Any]]) -> dict[str, Any]:
    walls = [result["wall_seconds"] for result in results]
    tool_calls = [result["agent_tool_calls"] for result in results]
    search_calls = [result["search_calls"] for result in results]
    inspection_calls = [result["inspection_calls"] for result in results]
    correct = sum(result["correct"] for result in results)
    return {
        "runs": len(results),
        "correct": correct,
        "accuracy": correct / len(results) if results else 0.0,
        "policy_violations": sum(result["policy_violation"] for result in results),
        "timeouts": sum(result["timed_out"] for result in results),
        "total_wall_seconds": sum(walls),
        "mean_wall_seconds": mean(walls),
        "median_wall_seconds": median(walls),
        "p95_wall_seconds": percentile(walls, 0.95),
        "total_agent_tool_calls": sum(tool_calls),
        "mean_agent_tool_calls": mean(tool_calls),
        "total_search_calls": sum(search_calls),
        "mean_search_calls": mean(search_calls),
        "total_inspection_calls": sum(inspection_calls),
        "mean_inspection_calls": mean(inspection_calls),
        "input_tokens": sum(
            result.get("usage", {}).get("input_tokens", 0) for result in results
        ),
        "cached_input_tokens": sum(
            result.get("usage", {}).get("cached_input_tokens", 0) for result in results
        ),
        "output_tokens": sum(
            result.get("usage", {}).get("output_tokens", 0) for result in results
        ),
    }


def reduction(baseline: list[float], awi: list[float]) -> float | None:
    baseline_total = sum(baseline)
    if baseline_total == 0:
        return None
    return 1.0 - sum(awi) / baseline_total


def mean(values: list[float]) -> float:
    return statistics.fmean(values) if values else 0.0


def median(values: list[float]) -> float:
    return statistics.median(values) if values else 0.0


def percentile(values: list[float], quantile: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = math.ceil((len(ordered) - 1) * quantile)
    return ordered[index]


def bootstrap_mean_ci(values: list[float], seed: int) -> list[float] | None:
    if not values:
        return None
    randomizer = random.Random(seed)
    samples = []
    for _ in range(5_000):
        sample = [randomizer.choice(values) for _ in values]
        samples.append(statistics.fmean(sample))
    samples.sort()
    return [samples[int(0.025 * len(samples))], samples[int(0.975 * len(samples))]]


def load_existing(path: Path) -> dict[str, dict[str, Any]]:
    if not path.is_file():
        return {}
    output = {}
    for line in path.read_text().splitlines():
        if line.strip():
            result = json.loads(line)
            output[
                result_key(result["task_id"], result["repetition"], result["arm"])
            ] = result
    return output


def result_key(task_id: str, repetition: int, arm: str) -> str:
    return f"{repetition}:{task_id}:{arm}"


def append_jsonl(path: Path, value: dict[str, Any]) -> None:
    with path.open("a") as handle:
        handle.write(
            json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n"
        )
        handle.flush()
        os.fsync(handle.fileno())


def write_json(path: Path, value: Any) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")
    temporary.replace(path)


def directory_size(path: Path) -> int:
    return sum(
        entry.stat().st_size
        for entry in path.rglob("*")
        if entry.is_file() and not entry.is_symlink()
    )


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        while chunk := handle.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def toml_string(value: object) -> str:
    return str(value).replace("\\", "\\\\").replace('"', '\\"')


if __name__ == "__main__":
    main()
