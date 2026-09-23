import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / "scripts" / "semantic_sidecar.py"
SPEC = importlib.util.spec_from_file_location("awi_semantic_sidecar", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
SIDECAR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SIDECAR)


class BalancedGroupsTest(unittest.TestCase):
    def test_preserves_order_without_empty_groups(self) -> None:
        for size in (1, 2, 3, 4, 5, 9, 17, 25, 31, 32):
            values = [str(index) for index in range(size)]
            groups = SIDECAR.balanced_groups(values, 4)
            self.assertEqual([item for group in groups for item in group], values)
            self.assertEqual(len(groups), min(size, 4))
            self.assertTrue(all(groups))

    def test_rejects_zero_groups(self) -> None:
        with self.assertRaisesRegex(ValueError, "positive"):
            SIDECAR.balanced_groups(["value"], 0)


if __name__ == "__main__":
    unittest.main()
