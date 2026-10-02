import copy
import json
import unittest
from pathlib import Path

from task_outcomes import apply_reviews, collect_tasks, summarize_tasks


def row(payload, second=0):
    return {
        "type": "response_item",
        "timestamp": f"2026-10-02T00:00:{second:02d}+00:00",
        "payload": payload,
    }


def message(role, text, **extra):
    return {"type": "message", "role": role, "content": [{"text": text}], **extra}


class TaskOutcomesTest(unittest.TestCase):
    def fixture(self):
        return collect_tasks(
            [
                {
                    "type": "session_meta",
                    "payload": {"id": "session", "cwd": "/workspace"},
                },
                row(message("user", "Find the retry behavior")),
                row(
                    {
                        "type": "function_call",
                        "namespace": "mcp__awi",
                        "name": "workspace_search",
                        "call_id": "call",
                        "arguments": '{"query":"retry"}',
                    },
                    1,
                ),
                row(
                    {
                        "type": "function_call_output",
                        "call_id": "call",
                        "output": {
                            "search_id": "s1",
                            "hits": [
                                {"path": "/workspace/retry.py", "preview": "retry=3"}
                            ],
                        },
                    },
                    2,
                ),
                *[
                    row(
                        {
                            "type": "function_call",
                            "name": "exec_command",
                            "arguments": json.dumps({"cmd": command}),
                        },
                        3 + i,
                    )
                    for i, command in enumerate(
                        ["true", "true", "true", "cat /workspace/retry.py"]
                    )
                ],
                row(
                    message(
                        "assistant",
                        "Evidence: /workspace/retry.py",
                        phase="final_answer",
                    ),
                    10,
                ),
                row(message("user", "Run a known command"), 11),
                row(
                    {
                        "type": "function_call",
                        "name": "exec_command",
                        "arguments": '{"cmd":"cat /workspace/retry.py"}',
                    },
                    12,
                ),
            ],
            "trace",
            Path("/workspace"),
            lambda value: (value, "visible"),
        )

    def review(self, task, reviewer="one", **labels):
        return {
            "task_id": task["task_id"],
            "reviewer_id": reviewer,
            "independent": True,
            "supporting_evidence": "final line and source retry definition agree",
            "retrieval_eligible": True,
            "answer_correct": True,
            "evidence_correct": True,
            "evidence_adopted": True,
            "adopted_search_keys": ["session:call"],
            **labels,
        }

    def test_turn_boundaries_and_late_followup_are_preserved(self):
        tasks = self.fixture()
        self.assertEqual(len(tasks), 2)
        search = tasks[0]["searches"][0]
        self.assertEqual(search["followup_action_count"], 4)
        self.assertEqual(search["path_reuse_candidates"], ["/workspace/retry.py"])
        self.assertEqual(search["final_path_candidates"], ["/workspace/retry.py"])
        self.assertEqual(tasks[0]["metrics"]["observed_turn_wall_seconds"], 10)
        self.assertIsNone(tasks[1]["metrics"]["observed_turn_wall_seconds"])
        self.assertIsNone(summarize_tasks(tasks)["correct_adoption_rate_on_known"])

    def test_dual_review_and_dispute(self):
        tasks = self.fixture()
        first = self.review(tasks[0])
        apply_reviews(tasks, [{"split": "dev", "reviews": [first]}])
        self.assertIsNone(summarize_tasks(tasks)["correct_adoption_rate_on_known"])
        second = self.review(tasks[0], "two")
        apply_reviews(tasks, [{"split": "dev", "reviews": [first, second]}])
        self.assertEqual(summarize_tasks(tasks)["correct_adoption_rate_on_known"], 1)
        self.assertEqual(
            summarize_tasks(tasks)["funnel_on_reviewed_eligible"][
                "adopted_without_inspect"
            ],
            1,
        )
        second["evidence_correct"] = False
        apply_reviews(tasks, [{"split": "dev", "reviews": [first, second]}])
        self.assertEqual(tasks[0]["review"]["status"], "disputed")
        self.assertIsNone(summarize_tasks(tasks)["correct_adoption_rate_on_known"])

    def test_unknowns_remain_unknown_and_invalid_reviews_fail(self):
        tasks = self.fixture()
        first = self.review(
            tasks[0],
            evidence_adopted=None,
            evidence_correct=None,
            adopted_search_keys=[],
        )
        second = {**first, "reviewer_id": "two"}
        apply_reviews(tasks, [{"split": "dev", "reviews": [first, second]}])
        summary = summarize_tasks(tasks)
        self.assertEqual(summary["correct_adoption_bounds_on_eligible"], [0, 1])
        self.assertEqual(summary["adoption_review_coverage"], 0)
        for document in [
            {"split": "frozen", "reviews": []},
            {"split": "dev", "reviews": [first, {**first, "reviewer_id": " ONE "}]},
            {"split": "dev", "reviews": [{**first, "task_id": "foreign"}]},
            {"split": "dev", "reviews": [{**first, "supporting_evidence": None}]},
            {"split": "dev", "reviews": [{**first, "evidence_adopted": True}]},
            {
                "split": "dev",
                "reviews": [{**first, "adopted_search_keys": ["foreign"]}],
            },
        ]:
            with self.assertRaises(ValueError):
                apply_reviews(copy.deepcopy(tasks), [document])


if __name__ == "__main__":
    unittest.main()
