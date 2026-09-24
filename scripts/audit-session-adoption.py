#!/usr/bin/env python3
"""Aggregate reviewed AWI session-adoption observations into a funnel report."""

from __future__ import annotations

import argparse
import json
import sys
from collections import defaultdict
from collections.abc import Iterable
from pathlib import Path
from typing import Any

SCHEMA_VERSION = 1
ELIGIBILITY_VALUES = {"eligible", "ineligible", "unknown"}
STAGES = (
    "tool_visible",
    "namespace_loaded",
    "search",
    "inspect",
    "evidence_adopted",
)
OBSERVATION_FIELDS = {
    "schema_version",
    "session_id",
    "client",
    "project_root",
    "eligibility",
    "eligibility_reason",
    "eligibility_review",
    "stages",
    "evidence_review",
}
REVIEW_FIELDS = {"status", "reviewers"}


class ObservationError(ValueError):
    """Raised when an observation cannot support an auditable funnel."""


def read_observations(paths: Iterable[Path]) -> list[dict[str, Any]]:
    observations: list[dict[str, Any]] = []
    seen: set[tuple[str, str, str]] = set()
    for path in paths:
        with path.open(encoding="utf-8") as handle:
            for line_number, line in enumerate(handle, 1):
                if not line.strip():
                    continue
                try:
                    value = json.loads(line)
                except json.JSONDecodeError as error:
                    raise ObservationError(
                        f"{path}:{line_number}: invalid JSON: {error}"
                    ) from error
                observation = validate_observation(value, path, line_number)
                key = (
                    observation["client"],
                    observation["session_id"],
                    observation["project_root"],
                )
                if key in seen:
                    raise ObservationError(
                        f"{path}:{line_number}: duplicate session observation {key!r}"
                    )
                seen.add(key)
                observations.append(observation)
    if not observations:
        raise ObservationError("no session observations found")
    return observations


def validate_observation(
    value: Any, source: Path | str = "<input>", line_number: int = 1
) -> dict[str, Any]:
    where = f"{source}:{line_number}"
    if not isinstance(value, dict):
        raise ObservationError(f"{where}: observation must be an object")
    if value.get("schema_version") != SCHEMA_VERSION:
        raise ObservationError(f"{where}: schema_version must be {SCHEMA_VERSION}")
    extra_fields = sorted(set(value) - OBSERVATION_FIELDS)
    if extra_fields:
        raise ObservationError(f"{where}: unknown fields: {extra_fields}")
    for field in ("session_id", "client", "project_root"):
        if not isinstance(value.get(field), str) or not value[field].strip():
            raise ObservationError(f"{where}: {field} must be a non-empty string")
    if not Path(value["project_root"]).is_absolute():
        raise ObservationError(f"{where}: project_root must be absolute")
    if "eligibility_reason" in value and not isinstance(
        value["eligibility_reason"], str
    ):
        raise ObservationError(f"{where}: eligibility_reason must be a string")
    eligibility = value.get("eligibility")
    if eligibility not in ELIGIBILITY_VALUES:
        raise ObservationError(
            f"{where}: eligibility must be one of {sorted(ELIGIBILITY_VALUES)}"
        )
    stages = value.get("stages")
    if not isinstance(stages, dict):
        raise ObservationError(f"{where}: stages must be an object")
    extra_stages = sorted(set(stages) - set(STAGES))
    if extra_stages:
        raise ObservationError(f"{where}: unknown stages: {extra_stages}")
    for stage in STAGES:
        if stage not in stages or not (
            isinstance(stages[stage], bool) or stages[stage] is None
        ):
            raise ObservationError(
                f"{where}: stages.{stage} must be true, false, or null"
            )

    require_implication(stages, "namespace_loaded", "tool_visible", where)
    require_implication(stages, "search", "namespace_loaded", where)
    require_implication(stages, "inspect", "search", where)
    require_implication(stages, "evidence_adopted", "search", where)
    validate_review(value.get("eligibility_review"), "eligibility_review", where)
    validate_review(value.get("evidence_review"), "evidence_review", where)
    return value


def require_implication(
    stages: dict[str, bool | None], child: str, parent: str, where: str
) -> None:
    if stages[child] is True and stages[parent] is not True:
        raise ObservationError(
            f"{where}: stages.{child}=true requires stages.{parent}=true"
        )


def validate_review(value: Any, field: str, where: str) -> None:
    if value is None:
        return
    if not isinstance(value, dict):
        raise ObservationError(f"{where}: {field} must be an object or null")
    extra_fields = sorted(set(value) - REVIEW_FIELDS)
    if extra_fields:
        raise ObservationError(f"{where}: unknown {field} fields: {extra_fields}")
    if value.get("status") not in {"candidate", "approved"}:
        raise ObservationError(f"{where}: {field}.status must be candidate or approved")
    reviewers = value.get("reviewers")
    if not isinstance(reviewers, list) or not all(
        isinstance(reviewer, str) and reviewer.strip() for reviewer in reviewers
    ):
        raise ObservationError(
            f"{where}: {field}.reviewers must contain non-empty strings"
        )


def aggregate(observations: list[dict[str, Any]]) -> dict[str, Any]:
    by_client: dict[str, list[dict[str, Any]]] = defaultdict(list)
    by_project: dict[str, list[dict[str, Any]]] = defaultdict(list)
    by_project_client: dict[str, dict[str, list[dict[str, Any]]]] = defaultdict(
        lambda: defaultdict(list)
    )
    for observation in observations:
        by_client[observation["client"]].append(observation)
        project_root = observation["project_root"]
        by_project[project_root].append(observation)
        by_project_client[project_root][observation["client"]].append(observation)

    review_gate = review_readiness(observations)
    return {
        "schema_version": SCHEMA_VERSION,
        "metric": "eligible_session_adoption",
        "report_status": ("approved" if review_gate["ready"] else "development_only"),
        "review_gate": review_gate,
        "overall": aggregate_group(observations),
        "by_client": {
            client: aggregate_group(items)
            for client, items in sorted(by_client.items())
        },
        "by_project": {
            project_root: aggregate_group(items)
            for project_root, items in sorted(by_project.items())
        },
        "by_project_client": {
            project_root: {
                client: aggregate_group(items)
                for client, items in sorted(client_items.items())
            }
            for project_root, client_items in sorted(by_project_client.items())
        },
    }


def aggregate_group(observations: list[dict[str, Any]]) -> dict[str, Any]:
    review_gate = review_readiness(observations)
    return {
        "report_status": ("approved" if review_gate["ready"] else "development_only"),
        "review_gate": review_gate,
        **aggregate_scope(observations),
    }


def aggregate_scope(observations: list[dict[str, Any]]) -> dict[str, Any]:
    eligible = [
        observation
        for observation in observations
        if observation["eligibility"] == "eligible"
    ]
    eligibility_counts = {
        status: sum(
            observation["eligibility"] == status for observation in observations
        )
        for status in ("eligible", "ineligible", "unknown")
    }
    raw_searches = sum(
        observation["stages"]["search"] is True for observation in observations
    )
    eligible_searches = sum(
        observation["stages"]["search"] is True for observation in eligible
    )

    funnel = {}
    previous_stage: str | None = None
    for stage in STAGES:
        denominator = (
            eligible
            if previous_stage is None
            else [
                observation
                for observation in eligible
                if observation["stages"][previous_stage] is True
            ]
        )
        reached = sum(
            observation["stages"][stage] is True for observation in denominator
        )
        not_reached = sum(
            observation["stages"][stage] is False for observation in denominator
        )
        unknown = len(denominator) - reached - not_reached
        funnel[stage] = {
            "denominator": len(denominator),
            "reached": reached,
            "not_reached": not_reached,
            "unknown": unknown,
            "conversion_rate": ratio(reached, len(denominator)),
        }
        if stage != "inspect":
            previous_stage = stage

    adopted_without_inspect = sum(
        observation["stages"]["evidence_adopted"] is True
        and observation["stages"]["inspect"] is False
        for observation in eligible
    )
    return {
        "sessions": len(observations),
        "eligibility": {
            **eligibility_counts,
            "labeled_coverage": ratio(
                eligibility_counts["eligible"] + eligibility_counts["ineligible"],
                len(observations),
            ),
        },
        "raw_session_adoption": {
            "numerator": raw_searches,
            "denominator": len(observations),
            "rate": ratio(raw_searches, len(observations)),
        },
        "eligible_session_adoption": {
            "numerator": eligible_searches,
            "denominator": len(eligible),
            "rate": ratio(eligible_searches, len(eligible)),
        },
        "funnel": funnel,
        "adopted_without_inspect": adopted_without_inspect,
    }


def review_readiness(observations: list[dict[str, Any]]) -> dict[str, Any]:
    reasons: dict[str, int] = defaultdict(int)
    if not any(
        observation["eligibility"] == "eligible" for observation in observations
    ):
        reasons["no_eligible_sessions"] = 1
    for observation in observations:
        if observation["eligibility"] == "unknown":
            reasons["unknown_eligibility"] += 1
        elif not dual_reviewed(observation.get("eligibility_review")):
            reasons["eligibility_not_dual_reviewed"] += 1

        stages = observation["stages"]
        if observation["eligibility"] != "eligible":
            continue
        previous_reached = True
        for stage in ("tool_visible", "namespace_loaded", "search", "inspect"):
            if previous_reached and stages[stage] is None:
                reasons[f"unknown_{stage}"] += 1
            if stage != "inspect":
                previous_reached = stages[stage] is True
        if stages["search"] is True:
            if stages["evidence_adopted"] is None:
                reasons["unknown_evidence_adoption"] += 1
            elif not dual_reviewed(observation.get("evidence_review")):
                reasons["evidence_not_dual_reviewed"] += 1
    return {
        "ready": not reasons,
        "required_reviewers": 2,
        "blocking_reasons": dict(sorted(reasons.items())),
    }


def dual_reviewed(value: Any) -> bool:
    if not isinstance(value, dict) or value.get("status") != "approved":
        return False
    reviewers = {
        reviewer.strip().casefold()
        for reviewer in value.get("reviewers", [])
        if isinstance(reviewer, str) and reviewer.strip()
    }
    return len(reviewers) >= 2


def ratio(numerator: int, denominator: int) -> float | None:
    if denominator == 0:
        return None
    return round(numerator / denominator, 6)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Aggregate AWI adoption over retrieval-eligible project sessions. "
            "Inputs are normalized JSONL observations."
        )
    )
    parser.add_argument(
        "sessions",
        nargs="+",
        type=Path,
        help="One or more session-observation JSONL files.",
    )
    parser.add_argument(
        "--output",
        type=Path,
        help="Write the report atomically instead of printing it.",
    )
    parser.add_argument("--compact", action="store_true", help="Emit compact JSON.")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        report = aggregate(read_observations(args.sessions))
    except (OSError, ObservationError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    text = json.dumps(
        report,
        ensure_ascii=False,
        indent=None if args.compact else 2,
        sort_keys=True,
    )
    if args.output is None:
        print(text)
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        temporary = args.output.with_suffix(args.output.suffix + ".tmp")
        temporary.write_text(text + "\n", encoding="utf-8")
        temporary.replace(args.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
