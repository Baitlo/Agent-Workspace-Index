from __future__ import annotations

import importlib.util
import json
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent


def load_runner():
    spec = importlib.util.spec_from_file_location(
        "awi_agent_ab_runner", HERE / "run_agent_ab.py"
    )
    if spec is None or spec.loader is None:
        raise RuntimeError("cannot load Agent A/B runner")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


RUNNER = load_runner()
ROOT = "/home/tiger/Projects/Bona"


class TaskSetGuardTests(unittest.TestCase):
    def test_candidate_tasks_require_draft_flag(self):
        with self.assertRaisesRegex(SystemExit, "--allow-draft"):
            RUNNER.validate_tasks_document(
                {
                    "split": "dev",
                    "review_status": "candidate_pending_dual_review",
                },
                allow_draft=False,
            )

    def test_candidate_tasks_are_allowed_for_pilot(self):
        RUNNER.validate_tasks_document(
            {
                "split": "dev",
                "review_status": "candidate_pending_dual_review",
            },
            allow_draft=True,
        )

    def test_frozen_tasks_are_rejected(self):
        with self.assertRaisesRegex(SystemExit, "frozen"):
            RUNNER.validate_tasks_document(
                {"split": "frozen", "review_status": "approved"},
                allow_draft=True,
            )

    def test_approved_dev_tasks_are_allowed(self):
        RUNNER.validate_tasks_document(
            {"split": "dev", "review_status": "approved"},
            allow_draft=False,
        )


class AnswerScoringTests(unittest.TestCase):
    def test_alternatives_match_either_exact_label_binding(self):
        task = {
            "expected_paths": ["result.json"],
            "required_answer_tokens": [
                "key=first.value|key=second.value",
            ],
        }
        final = {
            "answer": "key=second.value",
            "evidence_paths": [f"{ROOT}/result.json"],
        }
        self.assertEqual(
            RUNNER.score_answer(task, final, [ROOT]),
            (True, True),
        )

    def test_alternative_does_not_accept_unbound_value(self):
        task = {
            "expected_paths": ["result.json"],
            "required_answer_tokens": [
                "key=first.value|key=second.value",
            ],
        }
        final = {
            "answer": "key=wrong.value second.value",
            "evidence_paths": [f"{ROOT}/result.json"],
        }
        self.assertEqual(
            RUNNER.score_answer(task, final, [ROOT]),
            (True, False),
        )


class AwiScopeTests(unittest.TestCase):
    def test_null_hits_are_tolerated(self):
        events = [
            {
                "type": "item.completed",
                "item": {
                    "type": "mcp_tool_call",
                    "server": "awi",
                    "tool": "workspace_search",
                    "status": "failed",
                    "arguments": {"roots": [ROOT]},
                    "result": {"structured_content": {"hits": None}},
                },
            }
        ]
        metrics = RUNNER.tool_metrics(events)
        self.assertEqual(metrics["mcp_tools"][0]["hit_paths"], [])

    def test_search_requires_explicit_in_root_roots(self):
        calls = [
            {
                "server": "awi",
                "tool": "workspace_search",
                "arguments": {"query": "thing"},
            }
        ]
        self.assertEqual(
            RUNNER.find_awi_scope_violations(calls, [ROOT]),
            ["awi-search-missing-roots"],
        )

    def test_out_of_root_query_and_hit_are_detected(self):
        calls = [
            {
                "server": "awi",
                "tool": "workspace_query",
                "arguments": {
                    "roots": ["/tmp"],
                    "inputs": [{"path": "/etc/passwd", "alias": "bad"}],
                },
                "hit_paths": [],
            },
            {
                "server": "awi",
                "tool": "workspace_search",
                "arguments": {"roots": [ROOT]},
                "hit_paths": ["/tmp/leak.json"],
            },
        ]
        self.assertEqual(
            RUNNER.find_awi_scope_violations(calls, [ROOT]),
            [
                "awi-query-input:/etc/passwd",
                "awi-query-root:/tmp",
                "awi-result-path:/tmp/leak.json",
            ],
        )


class TaskBlindingTests(unittest.TestCase):
    def test_reviewed_answer_hints_are_removed(self):
        tasks = json.loads((HERE / "tasks.dev.json").read_text())["tasks"]
        questions = {task["id"]: task["question"] for task in tasks}
        self.assertNotIn("qwen_direct", questions["code-backend"])
        self.assertNotIn("500/500", questions["sql-training-optimization"])
        self.assertNotIn("completed", questions["result-v47-audit"].casefold())
        self.assertNotIn(
            "across three scenes",
            questions["result-v61-strict"].casefold(),
        )


if __name__ == "__main__":
    unittest.main()
