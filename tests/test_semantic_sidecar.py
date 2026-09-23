import importlib.util
import tempfile
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


class CoverageTest(unittest.TestCase):
    def test_returns_distinct_sorted_file_generations(self) -> None:
        class Column:
            def __init__(self, values: list[int]) -> None:
                self.values = values

            def to_pylist(self) -> list[int]:
                return self.values

        class Search:
            def select(self, _columns: list[str]) -> "Search":
                return self

            def to_arrow(self) -> dict[str, Column]:
                return {
                    "file_id": Column([9, 2, 9]),
                    "generation": Column([4, 7, 4]),
                }

        class Table:
            def search(self) -> Search:
                return Search()

            def count_rows(self) -> int:
                return 3

        class Tables:
            def __init__(self) -> None:
                self.tables = ["chunks"]

        class Database:
            def list_tables(self) -> Tables:
                return Tables()

            def open_table(self, _name: str) -> Table:
                return Table()

        class LanceDb:
            def connect(self, _path: Path) -> Database:
                return Database()

        sidecar = SIDECAR.Sidecar.__new__(SIDECAR.Sidecar)
        sidecar.lancedb = LanceDb()
        sidecar.tables = {}
        with tempfile.TemporaryDirectory() as directory:
            result = sidecar.coverage(Path(directory))
        self.assertEqual(result, {"pairs": [[2, 7], [9, 4]], "rows": 3})


class DocumentChunksTest(unittest.TestCase):
    def test_warm_runs_a_real_embedding_call(self) -> None:
        class Model:
            def embed(self, texts: list[str], *, normalize: bool) -> list[list[float]]:
                self.texts = texts
                self.normalize = normalize
                return [[0.0] * 640]

        sidecar = SIDECAR.Sidecar.__new__(SIDECAR.Sidecar)
        sidecar.model = Model()
        result = sidecar.handle({"op": "warm"})
        self.assertEqual(result, {"ready": True, "dimension": 640})
        self.assertTrue(sidecar.model.normalize)
        self.assertTrue(sidecar.model.texts[0].startswith(SIDECAR.QUERY_INSTRUCTION))

    def test_empty_payload_still_embeds_the_path_header(self) -> None:
        class ByteTokenizer:
            def tokenize(
                self, value: bytes, *, add_bos: bool, special: bool
            ) -> list[int]:
                self.options = (add_bos, special)
                return list(value)

            def detokenize(self, tokens: list[int]) -> bytes:
                return bytes(tokens)

        sidecar = SIDECAR.Sidecar.__new__(SIDECAR.Sidecar)
        sidecar.model = ByteTokenizer()
        sidecar.max_tokens = 480
        sidecar.max_chunks_per_file = 4
        chunks = sidecar.document_chunks(
            {
                "file_id": 7,
                "path": "/workspace/whitespace.txt",
                "kind": "text",
                "generation": 3,
                "content": "",
            }
        )
        self.assertEqual(len(chunks), 1)
        self.assertEqual(chunks[0]["file_id"], 7)
        self.assertIn("Path: /workspace/whitespace.txt", chunks[0]["text"])


if __name__ == "__main__":
    unittest.main()
