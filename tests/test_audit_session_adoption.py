import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / "scripts" / "audit-session-adoption.py"
SPEC = importlib.util.spec_from_file_location("audit_session_adoption", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
adoption = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(adoption)


def observation(
    session_id,
    client,
    eligibility,
    stages,
    *,
    eligibility_approved=True,
    evidence_approved=True,
):
    value = {
        "schema_version": 1,
        "session_id": session_id,
        "client": client,
        "project_root": "/workspace",
        "eligibility": eligibility,
        "eligibility_review": {
            "status": "approved" if eligibility_approved else "candidate",
            "reviewers": ["reviewer-a", "reviewer-b"],
        },
        "stages": stages,
    }
    if stages["evidence_adopted"] is not None:
        value["evidence_review"] = {
            "status": "approved" if evidence_approved else "candidate",
            "reviewers": ["reviewer-a", "reviewer-b"],
        }
    return value


class AdoptionFunnelTest(unittest.TestCase):
    def test_uses_only_eligible_sessions_for_primary_adoption(self):
        records = [
            observation(
                "codex-1",
                "codex",
                "eligible",
                {
                    "tool_visible": True,
                    "namespace_loaded": True,
                    "search": True,
                    "inspect": False,
                    "evidence_adopted": True,
                },
            ),
            observation(
                "codex-2",
                "codex",
                "eligible",
                {
                    "tool_visible": True,
                    "namespace_loaded": False,
                    "search": False,
                    "inspect": False,
                    "evidence_adopted": False,
                },
            ),
            observation(
                "zcode-1",
                "zcode",
                "eligible",
                {
                    "tool_visible": True,
                    "namespace_loaded": True,
                    "search": True,
                    "inspect": True,
                    "evidence_adopted": False,
                },
            ),
            observation(
                "zcode-2",
                "zcode",
                "ineligible",
                {
                    "tool_visible": True,
                    "namespace_loaded": True,
                    "search": False,
                    "inspect": False,
                    "evidence_adopted": False,
                },
            ),
            observation(
                "zcode-3",
                "zcode",
                "unknown",
                {
                    "tool_visible": None,
                    "namespace_loaded": None,
                    "search": False,
                    "inspect": False,
                    "evidence_adopted": None,
                },
            ),
        ]

        report = adoption.aggregate(records)
        overall = report["overall"]
        self.assertEqual(overall["sessions"], 5)
        self.assertEqual(overall["raw_session_adoption"]["rate"], 0.4)
        self.assertEqual(
            overall["eligible_session_adoption"],
            {"numerator": 2, "denominator": 3, "rate": 0.666667},
        )
        self.assertEqual(overall["funnel"]["namespace_loaded"]["denominator"], 3)
        self.assertEqual(overall["funnel"]["namespace_loaded"]["reached"], 2)
        self.assertEqual(overall["funnel"]["inspect"]["conversion_rate"], 0.5)
        self.assertEqual(overall["funnel"]["evidence_adopted"]["conversion_rate"], 0.5)
        self.assertEqual(overall["adopted_without_inspect"], 1)
        self.assertEqual(report["report_status"], "development_only")
        self.assertEqual(
            report["review_gate"]["blocking_reasons"],
            {"unknown_eligibility": 1},
        )
        self.assertEqual(
            report["by_project"]["/workspace"]["eligible_session_adoption"]["rate"],
            0.666667,
        )
        self.assertEqual(
            report["by_project"]["/workspace"]["report_status"], "development_only"
        )
        self.assertEqual(
            report["by_project_client"]["/workspace"]["codex"][
                "eligible_session_adoption"
            ]["rate"],
            0.5,
        )

    def test_marks_fully_dual_reviewed_observations_approved(self):
        records = [
            observation(
                "codex-1",
                "codex",
                "eligible",
                {
                    "tool_visible": True,
                    "namespace_loaded": True,
                    "search": True,
                    "inspect": True,
                    "evidence_adopted": True,
                },
            ),
            observation(
                "codex-2",
                "codex",
                "ineligible",
                {
                    "tool_visible": True,
                    "namespace_loaded": True,
                    "search": False,
                    "inspect": False,
                    "evidence_adopted": False,
                },
            ),
        ]

        report = adoption.aggregate(records)
        self.assertEqual(report["report_status"], "approved")
        self.assertTrue(report["review_gate"]["ready"])

    def test_rejects_non_monotonic_stages_and_duplicate_sessions(self):
        invalid = observation(
            "codex-1",
            "codex",
            "eligible",
            {
                "tool_visible": False,
                "namespace_loaded": True,
                "search": True,
                "inspect": False,
                "evidence_adopted": False,
            },
        )
        with self.assertRaisesRegex(
            adoption.ObservationError,
            "namespace_loaded=true requires stages.tool_visible=true",
        ):
            adoption.validate_observation(invalid)

        valid = observation(
            "codex-2",
            "codex",
            "eligible",
            {
                "tool_visible": True,
                "namespace_loaded": True,
                "search": False,
                "inspect": False,
                "evidence_adopted": False,
            },
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "sessions.jsonl"
            path.write_text(
                "\n".join(json.dumps(valid) for _ in range(2)) + "\n",
                encoding="utf-8",
            )
            with self.assertRaisesRegex(
                adoption.ObservationError, "duplicate session observation"
            ):
                adoption.read_observations([path])

    def test_cli_emits_development_only_when_a_funnel_stage_is_unknown(self):
        value = observation(
            "codex-1",
            "codex",
            "eligible",
            {
                "tool_visible": True,
                "namespace_loaded": True,
                "search": True,
                "inspect": None,
                "evidence_adopted": True,
            },
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "sessions.jsonl"
            path.write_text(json.dumps(value) + "\n", encoding="utf-8")
            completed = subprocess.run(
                [sys.executable, SCRIPT, path, "--compact"],
                check=True,
                capture_output=True,
                text=True,
            )
        report = json.loads(completed.stdout)
        self.assertEqual(report["report_status"], "development_only")
        self.assertEqual(
            report["review_gate"]["blocking_reasons"],
            {"unknown_inspect": 1},
        )


if __name__ == "__main__":
    unittest.main()
