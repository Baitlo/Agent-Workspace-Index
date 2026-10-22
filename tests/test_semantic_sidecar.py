import concurrent.futures
import importlib.util
import json
import sys
import tempfile
import threading
import time
import types
import unittest
from collections import OrderedDict
from pathlib import Path
from typing import ClassVar
from unittest import mock

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
            "semantic_snapshot_open_ms": 0.0,
            "vector_ms": 0.0,
            "serialization_ms": 0.0,
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


class OpenTableSemanticsTest(unittest.TestCase):
    def test_real_open_is_reported_only_on_cache_miss(self) -> None:
        opens: list[object] = []

        class Database:
            def list_tables(self) -> object:
                class Tables:
                    tables: ClassVar[list[str]] = ["chunks"]

                return Tables()

            def open_table(self, _name: str) -> object:
                opens.append("open_table")
                return object()

        class LanceDb:
            def connect(self, _path: Path) -> object:
                opens.append("connect")
                return Database()

        sidecar = SIDECAR.Sidecar.__new__(SIDECAR.Sidecar)
        sidecar.lancedb = LanceDb()
        sidecar.tables = {}
        sidecar.tables_lock = threading.Lock()
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory)
            first, opened_first, ms_first = sidecar.open_table(path)
            second, opened_second, ms_second = sidecar.open_table(path)
        self.assertEqual(opens, ["connect", "open_table"])
        self.assertTrue(opened_first)
        self.assertIsNotNone(ms_first)
        self.assertGreaterEqual(ms_first, 0.0)
        self.assertFalse(opened_second)
        self.assertIsNone(ms_second)
        self.assertIs(first, second)


class _FakeEmbedder:
    def embed(self, _text: str) -> tuple[list[float], dict[str, object]]:
        return [0.1, 0.2, 0.3], {
            "embedding_cache_hit": False,
            "coalesced": False,
            "embedding_queue_ms": 1.5,
            "embedding_ms": 2.5,
        }

    def clear(self) -> None:
        pass


class _RecordingSearch:
    def __init__(self) -> None:
        self.calls: list[tuple[object, ...]] = []

    def metric(self, value: str) -> object:
        self.calls.append(("metric", value))
        return self

    def where(self, clause: str, prefilter: bool) -> object:
        self.calls.append(("where", clause, prefilter))
        return self

    def nprobes(self, value: int) -> object:
        self.calls.append(("nprobes", value))
        return self

    def limit(self, value: int) -> object:
        self.calls.append(("limit", value))
        return self

    def to_list(self) -> list[dict[str, object]]:
        return [
            {"file_id": 1, "path": "a.rs", "generation": 3, "_distance": 0.2}
        ]


class _FakeTable:
    def __init__(self, search: _RecordingSearch) -> None:
        self.search_obj = search

    def search(self, vector: list[float], vector_column_name: str = "vector") -> _RecordingSearch:
        self.seen_vector = vector
        self.seen_column = vector_column_name
        return self.search_obj


class _FakeLanceDb:
    def __init__(self, table: object) -> None:
        self.table = table

    def connect(self, _path: Path) -> object:
        database = self

        class _Db:
            def list_tables(self) -> object:
                class Tables:
                    tables: ClassVar[list[str]] = ["chunks"]

                return Tables()

            def open_table(self, _name: str) -> object:
                return database.table

        return _Db()


class StageDiagnosticsTest(unittest.TestCase):
    def _build(self, index_type: str, partitions: int) -> tuple[object, _RecordingSearch]:
        search = _RecordingSearch()
        sidecar = SIDECAR.Sidecar.__new__(SIDECAR.Sidecar)
        sidecar.lancedb = _FakeLanceDb(_FakeTable(search))
        sidecar.tables = {}
        sidecar.tables_lock = threading.Lock()
        sidecar.query_embedder = _FakeEmbedder()
        sidecar.model_sha256 = None
        sidecar.max_chunks_per_file = 4
        sidecar.max_sql_chunks_per_file = 8
        return sidecar, search

    def _write_manifest(self, directory: str, index_type: str, partitions: int) -> Path:
        manifest = Path(directory) / "semantic.json"
        manifest.write_text(
            json.dumps(
                {
                    "generation": 3,
                    "vector_index_type": index_type,
                    "vector_index_partitions": partitions,
                }
            ),
            encoding="utf-8",
        )
        return manifest

    def test_nprobes_only_applied_on_sealed_ivf(self) -> None:
        cases = [
            ("IVF_FLAT", 16, True),
            ("FLAT", 0, False),
            ("IVF_FLAT", 0, False),
        ]
        for index_type, partitions, expect in cases:
            sidecar, search = self._build(index_type, partitions)
            with tempfile.TemporaryDirectory() as directory:
                manifest = self._write_manifest(directory, index_type, partitions)
                candidates, diagnostics = sidecar.query_uncached(
                    Path(directory), manifest, 3, "query", 5, [], ["text"], []
                )
            self.assertEqual(len(candidates), 1)
            self.assertEqual(diagnostics["index_type"], index_type)
            self.assertEqual(diagnostics["nprobes_applied"], expect)
            if expect:
                self.assertIn(("nprobes", partitions), search.calls)
            else:
                self.assertNotIn(("nprobes", partitions), search.calls)

    def test_snapshot_open_only_reports_real_open_events(self) -> None:
        sidecar, _search = self._build("IVF_FLAT", 16)
        with tempfile.TemporaryDirectory() as directory:
            manifest = self._write_manifest(directory, "IVF_FLAT", 16)
            _first, first_diag = sidecar.query_uncached(
                Path(directory), manifest, 3, "query", 5, [], ["text"], []
            )
            _second, second_diag = sidecar.query_uncached(
                Path(directory), manifest, 3, "other", 5, [], ["text"], []
            )
        self.assertTrue(first_diag["snapshot_opened"])
        self.assertIsNotNone(first_diag["semantic_snapshot_open_ms"])
        self.assertGreaterEqual(first_diag["semantic_snapshot_open_ms"], 0.0)
        self.assertFalse(second_diag["snapshot_opened"])
        self.assertIsNone(second_diag["semantic_snapshot_open_ms"])


class _IndexCaptureTable:
    def __init__(self, pairs: list[tuple[int, int]]) -> None:
        self.pairs = pairs
        self.created: dict[str, object] = {}

    def search(self, *args: object, **kwargs: object) -> object:
        return self

    def select(self, _columns: list[str]) -> object:
        return self

    def to_arrow(self) -> dict[str, object]:
        class Column:
            def __init__(self, values: list[int]) -> None:
                self.values = values

            def to_pylist(self) -> list[int]:
                return self.values

        return {
            "file_id": Column([pair[0] for pair in self.pairs]),
            "generation": Column([pair[1] for pair in self.pairs]),
        }

    def count_rows(self) -> int:
        return len(self.pairs)

    def delete(self, _predicate: str) -> None:
        pass

    def optimize(self, *args: object, **kwargs: object) -> None:
        pass

    def create_index(self, *args: object, **kwargs: object) -> None:
        self.created["args"] = args
        self.created["kwargs"] = kwargs


class IvfMigrationTest(unittest.TestCase):
    def test_seal_migrates_to_unified_create_index_api(self) -> None:
        pairs = [(index, 1) for index in range(300)]
        table = _IndexCaptureTable(pairs)
        sidecar = SIDECAR.Sidecar.__new__(SIDECAR.Sidecar)
        sidecar.lancedb = _FakeLanceDb(table)
        sidecar.tables = {}
        sidecar.tables_lock = threading.Lock()
        sidecar.model_sha256 = "test-sha"
        sidecar.model_path = "/tmp/test-model"
        sidecar.query_embedder = _FakeEmbedder()
        sidecar.query_lock = threading.Lock()
        sidecar.query_results = OrderedDict()
        sidecar.query_inflight = {}
        class FakeIvfFlat:
            def __init__(self, **kwargs: object) -> None:
                self.__dict__.update(kwargs)

        fake_package = types.ModuleType("lancedb")
        fake_package.__path__ = []
        fake_index = types.ModuleType("lancedb.index")
        fake_index.IvfFlat = FakeIvfFlat
        with (
            mock.patch.dict(
                sys.modules,
                {"lancedb": fake_package, "lancedb.index": fake_index},
            ),
            tempfile.TemporaryDirectory() as directory,
        ):
            sidecar.seal(
                Path(directory),
                Path(directory) / "semantic.json",
                1,
                [list(pair) for pair in pairs],
            )
        self.assertEqual(table.created.get("args"), ("vector",))
        config = table.created.get("kwargs", {}).get("config")
        self.assertIsNotNone(config)
        self.assertEqual(config.distance_type, "cosine")
        self.assertGreater(config.num_partitions, 0)


if __name__ == "__main__":
    unittest.main()
