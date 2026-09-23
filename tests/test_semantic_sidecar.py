import concurrent.futures
import importlib.util
import tempfile
import threading
import time
import unittest
from collections import OrderedDict
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


class QueryConcurrencyTest(unittest.TestCase):
    def test_query_embedder_batches_and_caches_parallel_requests(self) -> None:
        class Model:
            def __init__(self) -> None:
                self.calls: list[list[str]] = []

            def embed(self, texts: list[str], *, normalize: bool) -> list[list[float]]:
                self.calls.append(texts)
                return [[float(len(text))] for text in texts]

        model = Model()
        embedder = SIDECAR.QueryEmbedder(
            model,
            threading.Lock(),
            batch_size=4,
            batch_wait_ms=50,
            cache_size=8,
        )
        barrier = threading.Barrier(4)

        def embed(index: int) -> tuple[list[float], dict[str, object]]:
            barrier.wait()
            return embedder.embed(f"query-{index}")

        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
            results = list(executor.map(embed, range(4)))
        self.assertEqual(len(model.calls), 1)
        self.assertEqual(len(model.calls[0]), 4)
        self.assertTrue(all(not result[1]["embedding_cache_hit"] for result in results))

        _, diagnostics = embedder.embed("query-0")
        self.assertTrue(diagnostics["embedding_cache_hit"])
        self.assertEqual(len(model.calls), 1)

    def test_identical_queries_are_coalesced_before_result_cache(self) -> None:
        sidecar = SIDECAR.Sidecar.__new__(SIDECAR.Sidecar)
        sidecar.query_cache_size = 8
        sidecar.query_results = OrderedDict()
        sidecar.query_inflight = {}
        sidecar.query_lock = threading.Lock()
        sidecar.metrics_lock = threading.Lock()
        sidecar.metrics = {
            "queries": 0,
            "result_cache_hits": 0,
            "embedding_cache_hits": 0,
            "coalesced_queries": 0,
            "failures": 0,
            "queue_wait_ms": 0.0,
            "embedding_ms": 0.0,
            "vector_ms": 0.0,
            "total_ms": 0.0,
        }
        calls = 0
        calls_lock = threading.Lock()

        def query_uncached(
            *_args: object,
        ) -> tuple[list[dict[str, object]], dict[str, object]]:
            nonlocal calls
            with calls_lock:
                calls += 1
            time.sleep(0.05)
            return [{"file_id": 1}], {}

        sidecar.query_uncached = query_uncached
        barrier = threading.Barrier(4)

        def query(_: int) -> dict[str, object]:
            barrier.wait()
            return sidecar.query(
                Path("/tmp/index"),
                Path("/tmp/manifest"),
                3,
                "same query",
                10,
                [],
                [],
                [],
            )

        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as executor:
            results = list(executor.map(query, range(4)))
        self.assertEqual(calls, 1)
        self.assertTrue(
            all(result["candidates"] == [{"file_id": 1}] for result in results)
        )
        self.assertEqual(sidecar.metrics["coalesced_queries"], 3)


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
        sidecar.tables_lock = threading.Lock()
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
        sidecar.query_embedder = SIDECAR.QueryEmbedder(
            sidecar.model,
            threading.Lock(),
            batch_size=1,
            batch_wait_ms=0,
            cache_size=2,
        )
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
        sidecar.max_sql_chunks_per_file = 8
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

    def test_sql_chunks_preserve_structure_and_final_statement(self) -> None:
        class WordTokenizer:
            def tokenize(
                self, value: bytes, *, add_bos: bool, special: bool
            ) -> list[int]:
                self.values = value.decode("utf-8").split()
                return list(range(len(self.values)))

            def detokenize(self, tokens: list[int]) -> bytes:
                return " ".join(self.values[index] for index in tokens).encode()

        ctes = "\n".join(
            f"cte_{index} AS (SELECT {index} AS value)," for index in range(12)
        )
        content = (
            "WITH\n"
            f"{ctes}\n"
            "final_rows AS (SELECT value FROM cte_11)\n"
            "INSERT OVERWRITE DIRECTORY '/tmp/output'\n"
            "SELECT SUM(value) AS value_14d, MAX(value) AS value_60d\n"
            "FROM final_rows;\n"
        )
        sidecar = SIDECAR.Sidecar.__new__(SIDECAR.Sidecar)
        sidecar.model = WordTokenizer()
        sidecar.max_tokens = 120
        sidecar.max_chunks_per_file = 4
        sidecar.max_sql_chunks_per_file = 8
        chunks = sidecar.document_chunks(
            {
                "file_id": 8,
                "path": "/workspace/pair_labels.sql",
                "kind": "text",
                "generation": 4,
                "content": content,
            }
        )
        combined = "\n".join(chunk["text"] for chunk in chunks)
        self.assertEqual(len(chunks), 8)
        self.assertIn("SQL semantics:", combined)
        self.assertIn("materialized output", combined)
        self.assertIn("CTEs:", combined)
        self.assertIn("cte_11", combined)
        self.assertIn("INSERT OVERWRITE", combined)
        self.assertIn("value_14d", combined)
        self.assertIn("value_60d", combined)
        self.assertEqual(len({chunk["chunk_id"] for chunk in chunks}), len(chunks))


class SqlChunkingTest(unittest.TestCase):
    def test_summarizes_ranking_filters_and_identifiers(self) -> None:
        content = """
SELECT
    stid_v2,
    ROW_NUMBER() OVER (
        PARTITION BY advertiser_id, stid_v2
        ORDER BY cost DESC
    ) AS rn
FROM delivery
LEFT SEMI JOIN target_spus USING (stid_v2)
WHERE is_qianchuan_ad = 0
GROUP BY advertiser_id, stid_v2, cost
"""
        summary = SIDECAR.sql_structure_summary(content)
        self.assertIn("semi-join filtering", summary)
        self.assertIn("windowed row ranking", summary)
        self.assertIn("descending ranking by cost/spend", summary)
        self.assertIn("non-qianchuan ad rows", summary)
        self.assertIn("stid_v2", summary)

    def test_splits_top_level_ctes_but_not_nested_selects(self) -> None:
        content = """WITH
first AS (
    SELECT *
    FROM (
        SELECT 1 AS value
    ) nested
),
second AS (
    SELECT value
    FROM first
)
SELECT * FROM second;
"""
        sections = SIDECAR.sql_sections(content)
        self.assertEqual(len(sections), 3)
        self.assertIn("first AS", sections[0])
        self.assertIn("second AS", sections[1])
        self.assertTrue(sections[2].startswith("SELECT"))

    def test_representative_chunks_keep_first_and_last(self) -> None:
        chunks = [{"value": index} for index in range(12)]
        selected = SIDECAR.representative_chunks(chunks, 4)
        self.assertEqual([chunk["value"] for chunk in selected], [0, 4, 7, 11])


if __name__ == "__main__":
    unittest.main()
