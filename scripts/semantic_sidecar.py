"""Persistent Harrier GGUF embedding and LanceDB sidecar for AWI."""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import math
import os
import queue
import re
import shutil
import socket
import threading
import time
from collections import Counter, OrderedDict
from pathlib import Path
from typing import Any

QUERY_INSTRUCTION = (
    "Instruct: Given a software engineering query, retrieve the workspace code, "
    "configuration, dataset, or result that best answers it\nQuery: "
)
ELIGIBLE_KINDS = {"source", "text", "semi_structured", "tabular"}
CHUNKING_VERSION = 2


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--model-sha256")
    parser.add_argument("--threads", type=int, default=16)
    parser.add_argument("--batch-size", type=int, default=16)
    parser.add_argument("--embedding-workers", type=int, default=1)
    parser.add_argument("--max-tokens", type=int, default=480)
    parser.add_argument("--max-chunks-per-file", type=int, default=4)
    parser.add_argument("--max-sql-chunks-per-file", type=int, default=8)
    parser.add_argument("--query-batch-size", type=int, default=8)
    parser.add_argument("--query-batch-wait-ms", type=float, default=2.0)
    parser.add_argument("--query-cache-size", type=int, default=256)
    parser.add_argument("--max-inflight", type=int, default=32)
    parser.add_argument("--socket", type=Path, required=True)
    return parser.parse_args()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        while block := stream.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def sql_string(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def chunk_text(content: str, target_chars: int = 1_800) -> list[str]:
    lines = content.splitlines()
    chunks: list[str] = []
    current: list[str] = []
    current_chars = 0
    for line in lines:
        boundary = line.startswith(
            ("def ", "class ", "fn ", "pub fn ", "# ", "## ", "-- ")
        ) or line.rstrip().endswith(" AS (")
        if current and current_chars >= target_chars and boundary:
            chunks.append("\n".join(current).strip())
            current = current[-3:]
            current_chars = sum(len(item) + 1 for item in current)
        current.append(line)
        current_chars += len(line) + 1
        if current_chars >= target_chars * 2:
            chunks.append("\n".join(current).strip())
            current = current[-3:]
            current_chars = sum(len(item) + 1 for item in current)
    if current:
        chunks.append("\n".join(current).strip())
    return [chunk for chunk in chunks if chunk]


SQL_SECTION_RE = re.compile(
    r"^\s*(?:"
    r"(?:WITH\s+)?[A-Za-z_][A-Za-z0-9_$]*\s+AS\s*\("
    r"|INSERT(?:\s+OVERWRITE|\s+INTO)?\b"
    r"|CREATE(?:\s+OR\s+REPLACE)?\b"
    r"|MERGE\s+INTO\b"
    r"|SELECT\b"
    r")",
    re.IGNORECASE,
)
SQL_CTE_RE = re.compile(
    r"^\s*(?:WITH\s+)?([A-Za-z_][A-Za-z0-9_$]*)\s+AS\s*\(",
    re.IGNORECASE | re.MULTILINE,
)
SQL_RELATION_RE = re.compile(
    r"\b(?:FROM|JOIN|INTO|UPDATE|TABLE|VIEW)\s+"
    r"([`A-Za-z_][`A-Za-z0-9_.$/-]*)",
    re.IGNORECASE,
)
SQL_IDENTIFIER_RE = re.compile(r"\b[A-Za-z][A-Za-z0-9]*_[A-Za-z0-9_]+\b")
SQL_DESCENDING_RE = re.compile(
    r"\bORDER\s+BY\s+([A-Za-z_][A-Za-z0-9_.]*)\s+DESC\b",
    re.IGNORECASE,
)
SQL_DISABLED_FLAG_RE = re.compile(
    r"\b(?:[A-Za-z_][A-Za-z0-9_.]*\.)?is_([A-Za-z0-9_]+)\s*=\s*0\b",
    re.IGNORECASE,
)


def _sql_paren_delta(line: str) -> int:
    """Count structural parentheses outside quoted strings and line comments."""
    delta = 0
    quote: str | None = None
    escaped = False
    index = 0
    while index < len(line):
        char = line[index]
        if quote is None and char == "-" and line[index : index + 2] == "--":
            break
        if escaped:
            escaped = False
        elif quote is not None and char == "\\":
            escaped = True
        elif quote is not None and char == quote:
            quote = None
        elif quote is None and char in {"'", '"', "`"}:
            quote = char
        elif quote is None and char == "(":
            delta += 1
        elif quote is None and char == ")":
            delta -= 1
        index += 1
    return delta


def sql_sections(content: str) -> list[str]:
    lines = content.splitlines()
    sections: list[list[str]] = []
    current: list[str] = []
    depth = 0
    for line in lines:
        stripped = line.lstrip()
        boundary = depth == 0 and SQL_SECTION_RE.match(stripped) is not None
        if boundary and current and current[-1].strip().upper() == "WITH":
            boundary = False
        if (
            boundary
            and stripped.upper().startswith("SELECT")
            and any(
                previous.lstrip().upper().startswith(("INSERT ", "CREATE "))
                for previous in current
            )
        ):
            boundary = False
        if current and boundary:
            sections.append(current)
            current = []
        current.append(line)
        depth = max(0, depth + _sql_paren_delta(line))
    if current:
        sections.append(current)
    return ["\n".join(section).strip() for section in sections if any(section)]


def sql_structure_summary(content: str, max_items: int = 16) -> str:
    ctes = list(dict.fromkeys(SQL_CTE_RE.findall(content)))
    relations = [
        relation
        for relation in dict.fromkeys(SQL_RELATION_RE.findall(content))
        if relation.lower().strip("`") not in {"on", "select"}
    ]
    identifiers = [
        identifier
        for identifier, _ in Counter(
            match.lower() for match in SQL_IDENTIFIER_RE.findall(content)
        ).most_common(max_items)
    ]
    features = []
    normalized = " ".join(content.lower().split())
    if "left semi join" in normalized or "right semi join" in normalized:
        features.append("semi-join filtering through a target relation")
    if "row_number" in normalized and " over " in normalized:
        features.append("windowed row ranking")
    if "partition by" in normalized:
        features.append("partitioned window computation")
    for field in dict.fromkeys(SQL_DESCENDING_RE.findall(content)):
        label = field.rsplit(".", 1)[-1].replace("_", " ")
        if label == "cost":
            label = "cost/spend"
        features.append(f"descending ranking by {label}")
    for flag in dict.fromkeys(SQL_DISABLED_FLAG_RE.findall(content)):
        features.append(f"non-{flag.replace('_', ' ')} rows")
    if "group by" in normalized:
        features.append("grouped aggregation")
    if "insert overwrite" in normalized:
        features.append("materialized output")
    parts = []
    if features:
        parts.append("SQL semantics: " + "; ".join(features[:max_items]))
    if ctes:
        parts.append("CTEs: " + ", ".join(ctes[:max_items]))
    if relations:
        parts.append("Relations: " + ", ".join(relations[:max_items]))
    if identifiers:
        parts.append("Key identifiers: " + ", ".join(identifiers))
    return "\n".join(parts)


def representative_chunks(
    chunks: list[dict[str, Any]], limit: int
) -> list[dict[str, Any]]:
    if len(chunks) <= limit:
        return chunks
    if limit == 1:
        return [chunks[-1]]
    indexes = {
        round(position * (len(chunks) - 1) / (limit - 1)) for position in range(limit)
    }
    return [chunks[index] for index in sorted(indexes)]


def balanced_groups(values: list[str], group_count: int) -> list[list[str]]:
    if group_count < 1:
        raise ValueError("group_count must be positive")
    active_groups = min(group_count, len(values))
    if active_groups == 0:
        return []
    base_size, extra = divmod(len(values), active_groups)
    groups = []
    offset = 0
    for index in range(active_groups):
        group_size = base_size + int(index < extra)
        groups.append(values[offset : offset + group_size])
        offset += group_size
    return groups


class EmbeddingJob:
    def __init__(self, text: str) -> None:
        self.text = text
        self.submitted_at = time.perf_counter()
        self.started_at = self.submitted_at
        self.finished_at = self.submitted_at
        self.vector: list[float] | None = None
        self.error: BaseException | None = None
        self.event = threading.Event()


class QueryEmbedder:
    def __init__(
        self,
        model: Any,
        model_lock: threading.Lock,
        batch_size: int,
        batch_wait_ms: float,
        cache_size: int,
    ) -> None:
        self.model = model
        self.model_lock = model_lock
        self.batch_size = batch_size
        self.batch_wait_seconds = batch_wait_ms / 1_000.0
        self.cache_size = cache_size
        self.pending: queue.Queue[EmbeddingJob] = queue.Queue()
        self.cache: OrderedDict[str, list[float]] = OrderedDict()
        self.inflight: dict[str, EmbeddingJob] = {}
        self.lock = threading.Lock()
        threading.Thread(
            target=self._run,
            name="awi-query-embedder",
            daemon=True,
        ).start()

    def embed(self, text: str) -> tuple[list[float], dict[str, Any]]:
        requested_at = time.perf_counter()
        with self.lock:
            cached = self.cache.get(text)
            if cached is not None:
                self.cache.move_to_end(text)
                return cached, {
                    "embedding_cache_hit": True,
                    "coalesced": False,
                    "queue_wait_ms": 0.0,
                    "embedding_ms": 0.0,
                }
            job = self.inflight.get(text)
            coalesced = job is not None
            if job is None:
                job = EmbeddingJob(text)
                self.inflight[text] = job
                self.pending.put(job)
        job.event.wait()
        if job.error is not None:
            raise RuntimeError(f"query embedding failed: {job.error}") from job.error
        if job.vector is None:
            raise RuntimeError("query embedding completed without a vector")
        return job.vector, {
            "embedding_cache_hit": False,
            "coalesced": coalesced,
            "queue_wait_ms": max(0.0, (job.started_at - requested_at) * 1_000),
            "embedding_ms": max(0.0, (job.finished_at - job.started_at) * 1_000),
        }

    def clear(self) -> None:
        with self.lock:
            self.cache.clear()

    def _run(self) -> None:
        while True:
            first = self.pending.get()
            jobs = [first]
            deadline = time.perf_counter() + self.batch_wait_seconds
            while len(jobs) < self.batch_size:
                remaining = deadline - time.perf_counter()
                if remaining <= 0:
                    break
                try:
                    jobs.append(self.pending.get(timeout=remaining))
                except queue.Empty:
                    break
            started_at = time.perf_counter()
            for job in jobs:
                job.started_at = started_at
            try:
                with self.model_lock:
                    vectors = self.model.embed(
                        [job.text for job in jobs],
                        normalize=True,
                    )
                if len(vectors) != len(jobs):
                    raise RuntimeError(
                        f"embedding batch returned {len(vectors)} vectors "
                        f"for {len(jobs)} queries"
                    )
                finished_at = time.perf_counter()
                with self.lock:
                    for job, vector in zip(jobs, vectors, strict=True):
                        job.vector = vector
                        job.finished_at = finished_at
                        self.cache[job.text] = vector
                        self.cache.move_to_end(job.text)
                        self.inflight.pop(job.text, None)
                    while len(self.cache) > self.cache_size:
                        self.cache.popitem(last=False)
            except BaseException as error:  # noqa: BLE001 - worker boundary
                finished_at = time.perf_counter()
                with self.lock:
                    for job in jobs:
                        job.error = error
                        job.finished_at = finished_at
                        self.inflight.pop(job.text, None)
            finally:
                for job in jobs:
                    job.event.set()


class QueryResultJob:
    def __init__(self) -> None:
        self.result: list[dict[str, Any]] | None = None
        self.error: BaseException | None = None
        self.event = threading.Event()


class Sidecar:
    def __init__(self, args: argparse.Namespace) -> None:
        import lancedb
        from llama_cpp import Llama

        if args.model_sha256:
            actual = sha256_file(args.model)
            if actual != args.model_sha256:
                raise RuntimeError(
                    f"model checksum mismatch: expected {args.model_sha256}, got {actual}"
                )
        self.lancedb = lancedb
        self.model_path = args.model.resolve()
        self.model_sha256 = args.model_sha256
        self.threads = args.threads
        self.batch_size = args.batch_size
        self.embedding_workers = args.embedding_workers
        self.max_tokens = args.max_tokens
        self.max_chunks_per_file = args.max_chunks_per_file
        self.max_sql_chunks_per_file = args.max_sql_chunks_per_file
        self.query_batch_size = args.query_batch_size
        self.query_batch_wait_ms = args.query_batch_wait_ms
        self.query_cache_size = args.query_cache_size
        self.max_inflight = args.max_inflight
        if self.threads < 1:
            raise ValueError("threads must be positive")
        if self.embedding_workers < 1:
            raise ValueError("embedding-workers must be positive")
        if self.max_chunks_per_file < 1 or self.max_sql_chunks_per_file < 1:
            raise ValueError("chunk limits must be positive")
        if self.query_batch_size < 1 or self.max_inflight < 1:
            raise ValueError("query concurrency limits must be positive")
        if self.query_batch_wait_ms < 0:
            raise ValueError("query-batch-wait-ms must be non-negative")
        if self.query_cache_size < 1:
            raise ValueError("query-cache-size must be positive")
        self.llama = Llama
        self.model = self.create_model(self.threads, 512)
        self.model_lock = threading.Lock()
        self.query_embedder = QueryEmbedder(
            self.model,
            self.model_lock,
            self.query_batch_size,
            self.query_batch_wait_ms,
            self.query_cache_size,
        )
        self.update_models: list[Any] | None = None
        self.tables: dict[str, Any] = {}
        self.tables_lock = threading.Lock()
        self.query_results: OrderedDict[tuple[Any, ...], list[dict[str, Any]]] = (
            OrderedDict()
        )
        self.query_inflight: dict[tuple[Any, ...], QueryResultJob] = {}
        self.query_lock = threading.Lock()
        self.metrics_lock = threading.Lock()
        self.metrics = {
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

    def create_model(self, threads: int, n_batch: int) -> Any:
        return self.llama(
            model_path=str(self.model_path),
            embedding=True,
            n_ctx=512,
            n_batch=n_batch,
            n_ubatch=512,
            n_threads=threads,
            n_threads_batch=threads,
            verbose=False,
        )

    def document_embedding_models(self) -> list[Any]:
        if self.embedding_workers == 1:
            return [self.model]
        if self.update_models is None:
            threads = max(1, self.threads // self.embedding_workers)
            self.update_models = [
                self.create_model(threads, 512) for _ in range(self.embedding_workers)
            ]
        return self.update_models

    def embed_documents(self, texts: list[str]) -> list[list[float]]:
        models = self.document_embedding_models()
        active_workers = min(len(models), len(texts))
        if active_workers == 1:
            if models[0] is self.model:
                with self.model_lock:
                    return models[0].embed(texts, normalize=True)
            return models[0].embed(texts, normalize=True)
        groups = balanced_groups(texts, active_workers)
        with concurrent.futures.ThreadPoolExecutor(
            max_workers=active_workers
        ) as executor:
            futures = [
                executor.submit(model.embed, group, normalize=True)
                for model, group in zip(models[:active_workers], groups, strict=True)
            ]
            embedded = []
            for index, (future, group) in enumerate(zip(futures, groups, strict=True)):
                try:
                    embedded.append(future.result())
                except Exception as error:
                    token_counts = [
                        len(
                            self.model.tokenize(
                                text.encode("utf-8"),
                                add_bos=True,
                                special=True,
                            )
                        )
                        for text in group
                    ]
                    raise RuntimeError(
                        "parallel document embedding failed for "
                        f"group={index} texts={len(group)} "
                        f"tokens={token_counts}: {error}"
                    ) from error
            return [vector for group in embedded for vector in group]

    def handle(self, request: dict[str, Any]) -> dict[str, Any]:
        operation = request.get("op")
        if operation == "ping":
            return {"ready": True}
        if operation == "warm":
            vector, _ = self.query_embedder.embed(
                QUERY_INSTRUCTION + "warm semantic retrieval"
            )
            return {"ready": True, "dimension": len(vector)}
        if operation == "metrics":
            with self.metrics_lock:
                return dict(self.metrics)
        if operation == "reset":
            return self.reset(Path(request["database"]))
        if operation == "coverage":
            return self.coverage(Path(request["database"]))
        if operation == "update":
            return self.update(
                Path(request["database"]),
                request["documents"],
                request.get("deleted_paths", []),
            )
        if operation == "seal":
            return self.seal(
                Path(request["database"]),
                Path(request["manifest"]),
                int(request["generation"]),
                request["expected"],
            )
        if operation == "query":
            return self.query(
                Path(request["database"]),
                Path(request["manifest"]),
                int(request["generation"]),
                str(request["query"]),
                int(request["limit"]),
                request.get("roots", []),
                request.get("kinds", []),
                request.get("path_prefixes", []),
            )
        raise ValueError(f"unknown operation: {operation}")

    def reset(self, database: Path) -> dict[str, Any]:
        with self.tables_lock:
            self.tables.pop(str(database.resolve()), None)
        self.clear_query_caches()
        if database.exists():
            shutil.rmtree(database)
        return {"reset": True}

    def document_chunks(self, document: dict[str, Any]) -> list[dict[str, Any]]:
        if document["kind"] not in ELIGIBLE_KINDS:
            return []
        header = f"Path: {document['path']}\nFile type: {document['kind']}\n"
        is_sql = Path(document["path"]).suffix.lower() == ".sql"
        sections = (
            sql_sections(document["content"])
            if is_sql
            else chunk_text(document["content"])
        )
        if is_sql:
            summary = sql_structure_summary(document["content"])
            if summary:
                header += summary + "\n"
        chunks: list[dict[str, Any]] = []
        for section in sections or [""]:
            text = header + section
            tokens = self.model.tokenize(
                text.encode("utf-8"), add_bos=False, special=False
            )
            for offset in range(0, len(tokens), self.max_tokens):
                window = tokens[offset : offset + self.max_tokens]
                if not window:
                    continue
                chunk_text_value = self.model.detokenize(window).decode(
                    "utf-8", errors="replace"
                )
                chunk_index = len(chunks)
                chunk_id = hashlib.sha256(
                    (
                        f"{document['file_id']}\0{document['generation']}\0"
                        f"{chunk_index}\0{chunk_text_value}"
                    ).encode()
                ).hexdigest()[:32]
                chunks.append(
                    {
                        "chunk_id": chunk_id,
                        "file_id": int(document["file_id"]),
                        "path": document["path"],
                        "kind": document["kind"],
                        "generation": int(document["generation"]),
                        "text": chunk_text_value,
                    }
                )
        limit = self.max_sql_chunks_per_file if is_sql else self.max_chunks_per_file
        selected = representative_chunks(chunks, limit)
        for chunk_index, chunk in enumerate(selected):
            chunk["chunk_id"] = hashlib.sha256(
                (
                    f"{document['file_id']}\0{document['generation']}\0"
                    f"{chunk_index}\0{chunk['text']}"
                ).encode()
            ).hexdigest()[:32]
        return selected

    def update(
        self,
        database: Path,
        documents: list[dict[str, Any]],
        deleted_paths: list[str],
    ) -> dict[str, Any]:
        import pyarrow as pa

        database.parent.mkdir(parents=True, exist_ok=True)
        rows: list[dict[str, Any]] = []
        for document in documents:
            rows.extend(self.document_chunks(document))

        if rows:
            texts = [row.pop("text") for row in rows]
            vectors: list[list[float]] = []
            for start in range(0, len(texts), self.batch_size):
                vectors.extend(
                    self.embed_documents(texts[start : start + self.batch_size])
                )
            dimension = len(vectors[0])
            for row, vector in zip(rows, vectors, strict=True):
                row["vector"] = vector
            schema = pa.schema(
                [
                    pa.field("chunk_id", pa.string()),
                    pa.field("file_id", pa.int64()),
                    pa.field("path", pa.string()),
                    pa.field("kind", pa.string()),
                    pa.field("generation", pa.int64()),
                    pa.field("vector", pa.list_(pa.float32(), dimension)),
                ]
            )
            batch = pa.Table.from_pylist(rows, schema=schema)
        else:
            batch = None
            dimension = 640
        key = str(database.resolve())
        db = self.lancedb.connect(database)
        if "chunks" in db.list_tables().tables:
            table = db.open_table("chunks")
            replaced_paths = sorted(
                set(deleted_paths) | {document["path"] for document in documents}
            )
            for start in range(0, len(replaced_paths), 100):
                paths = replaced_paths[start : start + 100]
                predicate = "path IN (" + ",".join(map(sql_string, paths)) + ")"
                table.delete(predicate)
            if batch is not None:
                table.add(batch, mode="append")
        elif batch is not None:
            table = db.create_table("chunks", data=batch, mode="create")
        else:
            return {"files": 0, "chunks": 0, "dimension": dimension}
        with self.tables_lock:
            self.tables[key] = table
        self.clear_query_caches()
        return {
            "files": len({row["file_id"] for row in rows}),
            "chunks": len(rows),
            "dimension": dimension,
        }

    def open_table(self, database: Path) -> Any:
        key = str(database.resolve())
        with self.tables_lock:
            table = self.tables.get(key)
        if table is None:
            db = self.lancedb.connect(database)
            if "chunks" not in db.list_tables().tables:
                raise RuntimeError(f"semantic table is missing under {database}")
            table = db.open_table("chunks")
            with self.tables_lock:
                table = self.tables.setdefault(key, table)
        return table

    def coverage(self, database: Path) -> dict[str, Any]:
        if not database.is_dir():
            return {"pairs": [], "rows": 0}
        db = self.lancedb.connect(database)
        if "chunks" not in db.list_tables().tables:
            return {"pairs": [], "rows": 0}
        table = self.open_table(database)
        rows = table.search().select(["file_id", "generation"]).to_arrow()
        pairs = sorted(
            set(
                zip(
                    rows["file_id"].to_pylist(),
                    rows["generation"].to_pylist(),
                    strict=True,
                )
            )
        )
        return {
            "pairs": [[file_id, generation] for file_id, generation in pairs],
            "rows": table.count_rows(),
        }

    def seal(
        self,
        database: Path,
        manifest: Path,
        generation: int,
        expected: list[list[int]],
    ) -> dict[str, Any]:
        table = self.open_table(database)
        expected_pairs = {
            (int(file_id), int(index_generation))
            for file_id, index_generation in expected
        }
        rows = table.search().select(["file_id", "generation"]).to_arrow()
        actual_pairs = set(
            zip(
                rows["file_id"].to_pylist(),
                rows["generation"].to_pylist(),
                strict=True,
            )
        )
        missing = sorted(expected_pairs - actual_pairs)
        if missing:
            preview = missing[:20]
            raise RuntimeError(
                f"semantic coverage missing {len(missing)} file generations: {preview}"
            )
        unexpected = sorted(actual_pairs - expected_pairs)
        if unexpected:
            for start in range(0, len(unexpected), 100):
                pairs = unexpected[start : start + 100]
                predicate = " OR ".join(
                    f"(file_id = {file_id} AND generation = {file_generation})"
                    for file_id, file_generation in pairs
                )
                table.delete(predicate)
            actual_pairs.difference_update(unexpected)
        if actual_pairs != expected_pairs:
            raise RuntimeError("semantic coverage does not match the catalog")

        row_count = table.count_rows()
        index_type = "IVF_FLAT" if row_count >= 256 else "FLAT"
        partitions = (
            min(256, max(16, int(math.sqrt(row_count)))) if row_count >= 256 else 0
        )
        existing = None
        if manifest.is_file():
            existing = json.loads(manifest.read_text(encoding="utf-8"))
        index_is_current = (
            existing is not None
            and int(existing.get("files", -1)) == len(expected_pairs)
            and int(existing.get("rows", -1)) == row_count
            and existing.get("model_sha256") == self.model_sha256
            and int(existing.get("chunking_version", 0)) == CHUNKING_VERSION
            and existing.get("vector_index_type") == index_type
            and int(existing.get("vector_index_partitions", -1)) == partitions
        )
        already_sealed = (
            index_is_current and int(existing.get("generation", -1)) == generation
        )
        if row_count >= 256 and not index_is_current:
            table.create_index(
                metric="cosine",
                vector_column_name="vector",
                index_type="IVF_FLAT",
                num_partitions=partitions,
                replace=True,
            )
        if already_sealed:
            return existing
        payload = {
            "format_version": 1,
            "generation": generation,
            "model_path": str(self.model_path),
            "model_sha256": self.model_sha256,
            "chunking_version": CHUNKING_VERSION,
            "dimension": 640,
            "files": len(expected_pairs),
            "rows": row_count,
            "vector_index_type": index_type,
            "vector_index_partitions": partitions,
            "pruned_file_generations": len(unexpected),
            "created_at_ms": int(time.time() * 1000),
        }
        manifest.parent.mkdir(parents=True, exist_ok=True)
        temporary = manifest.with_name(f".{manifest.name}.tmp-{os.getpid()}")
        with temporary.open("w", encoding="utf-8") as stream:
            json.dump(payload, stream, ensure_ascii=False, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, manifest)
        self.clear_query_caches()
        return payload

    def clear_query_caches(self) -> None:
        self.query_embedder.clear()
        with self.query_lock:
            self.query_results.clear()

    def record_query_metrics(self, diagnostics: dict[str, Any], failed: bool) -> None:
        with self.metrics_lock:
            self.metrics["queries"] += 1
            self.metrics["failures"] += int(failed)
            self.metrics["result_cache_hits"] += int(
                diagnostics.get("result_cache_hit", False)
            )
            self.metrics["embedding_cache_hits"] += int(
                diagnostics.get("embedding_cache_hit", False)
            )
            self.metrics["coalesced_queries"] += int(
                diagnostics.get("coalesced", False)
            )
            for key in (
                "queue_wait_ms",
                "embedding_ms",
                "vector_ms",
                "total_ms",
            ):
                self.metrics[key] += float(diagnostics.get(key, 0.0))

    def query(
        self,
        database: Path,
        manifest: Path,
        generation: int,
        query: str,
        limit: int,
        roots: list[str],
        kinds: list[str],
        path_prefixes: list[str],
    ) -> dict[str, Any]:
        started_at = time.perf_counter()
        cache_key = (
            str(database.resolve()),
            generation,
            query,
            limit,
            tuple(roots),
            tuple(kinds),
            tuple(path_prefixes),
        )
        with self.query_lock:
            cached = self.query_results.get(cache_key)
            if cached is not None:
                self.query_results.move_to_end(cache_key)
            job = self.query_inflight.get(cache_key)
            coalesced_result = job is not None
            if cached is None and job is None:
                job = QueryResultJob()
                self.query_inflight[cache_key] = job
        if cached is not None:
            diagnostics = {
                "result_cache_hit": True,
                "embedding_cache_hit": True,
                "coalesced": False,
                "queue_wait_ms": 0.0,
                "embedding_ms": 0.0,
                "vector_ms": 0.0,
                "total_ms": (time.perf_counter() - started_at) * 1_000,
            }
            self.record_query_metrics(diagnostics, False)
            return {"candidates": cached, "diagnostics": diagnostics}
        if job is None:
            raise RuntimeError("semantic query coalescing state is missing")
        if coalesced_result:
            job.event.wait()
            if job.error is not None:
                raise RuntimeError(
                    f"coalesced semantic query failed: {job.error}"
                ) from (job.error)
            if job.result is None:
                raise RuntimeError(
                    "coalesced semantic query completed without a result"
                )
            diagnostics = {
                "result_cache_hit": False,
                "embedding_cache_hit": False,
                "coalesced": True,
                "queue_wait_ms": 0.0,
                "embedding_ms": 0.0,
                "vector_ms": 0.0,
                "total_ms": (time.perf_counter() - started_at) * 1_000,
            }
            self.record_query_metrics(diagnostics, False)
            return {"candidates": job.result, "diagnostics": diagnostics}

        diagnostics: dict[str, Any] = {
            "result_cache_hit": False,
            "embedding_cache_hit": False,
            "coalesced": False,
            "queue_wait_ms": 0.0,
            "embedding_ms": 0.0,
            "vector_ms": 0.0,
        }
        try:
            candidates, query_diagnostics = self.query_uncached(
                database,
                manifest,
                generation,
                query,
                limit,
                roots,
                kinds,
                path_prefixes,
            )
            diagnostics.update(query_diagnostics)
            with self.query_lock:
                job.result = candidates
                self.query_results[cache_key] = candidates
                self.query_results.move_to_end(cache_key)
                while len(self.query_results) > self.query_cache_size:
                    self.query_results.popitem(last=False)
            return {"candidates": candidates, "diagnostics": diagnostics}
        except BaseException as error:
            job.error = error
            raise
        finally:
            diagnostics["total_ms"] = (time.perf_counter() - started_at) * 1_000
            self.record_query_metrics(diagnostics, job.error is not None)
            with self.query_lock:
                self.query_inflight.pop(cache_key, None)
            job.event.set()

    def query_uncached(
        self,
        database: Path,
        manifest: Path,
        generation: int,
        query: str,
        limit: int,
        roots: list[str],
        kinds: list[str],
        path_prefixes: list[str],
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        metadata = json.loads(manifest.read_text(encoding="utf-8"))
        if int(metadata["generation"]) != generation:
            raise RuntimeError(
                f"semantic generation {metadata['generation']} != catalog {generation}"
            )
        if self.model_sha256 and metadata.get("model_sha256") != self.model_sha256:
            raise RuntimeError(
                "semantic manifest model checksum does not match runtime"
            )
        vector, diagnostics = self.query_embedder.embed(QUERY_INSTRUCTION + query)
        predicates = [f"generation <= {generation}"]
        if roots:
            root_filters = [
                f"(path = {sql_string(root)} OR path LIKE {sql_string(root.rstrip('/') + '/%')})"
                for root in roots
            ]
            predicates.append("(" + " OR ".join(root_filters) + ")")
        eligible_kinds = sorted(set(kinds) & ELIGIBLE_KINDS) if kinds else []
        if kinds and not eligible_kinds:
            return [], diagnostics
        if eligible_kinds:
            predicates.append(
                "kind IN (" + ",".join(map(sql_string, eligible_kinds)) + ")"
            )
        if path_prefixes:
            prefix_filters = [
                f"(path = {sql_string(prefix)} OR "
                f"path LIKE {sql_string(prefix.rstrip('/') + '/%')})"
                for prefix in path_prefixes
            ]
            predicates.append("(" + " OR ".join(prefix_filters) + ")")
        search = (
            self.open_table(database)
            .search(vector, vector_column_name="vector")
            .metric("cosine")
            .where(" AND ".join(predicates), prefilter=True)
        )
        partitions = int(metadata.get("vector_index_partitions", 0))
        if partitions:
            search = search.nprobes(partitions)
        vector_started_at = time.perf_counter()
        chunks_per_file = max(self.max_chunks_per_file, self.max_sql_chunks_per_file)
        rows = search.limit(max(limit * chunks_per_file, limit)).to_list()
        diagnostics["vector_ms"] = (time.perf_counter() - vector_started_at) * 1_000
        candidates: list[dict[str, Any]] = []
        seen: set[int] = set()
        for row in rows:
            file_id = int(row["file_id"])
            if file_id in seen:
                continue
            seen.add(file_id)
            candidates.append(
                {
                    "file_id": file_id,
                    "path": row["path"],
                    "generation": int(row["generation"]),
                    "score": 1.0 - float(row["_distance"]),
                }
            )
            if len(candidates) >= limit:
                break
        return candidates, diagnostics


def response_bytes(value: dict[str, Any]) -> bytes:
    return (
        json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n"
    ).encode()


def terminate_with_parent() -> None:
    """Ask Linux to terminate the sidecar when its Rust parent exits."""
    import ctypes
    import signal

    libc = ctypes.CDLL(None, use_errno=True)
    pr_set_pdeathsig = 1
    if libc.prctl(pr_set_pdeathsig, signal.SIGTERM, 0, 0, 0) != 0:
        error = ctypes.get_errno()
        raise OSError(error, "prctl(PR_SET_PDEATHSIG) failed")
    if os.getppid() == 1:
        os.kill(os.getpid(), signal.SIGTERM)


def handle_connection(sidecar: Sidecar, connection: socket.socket) -> None:
    with connection:
        try:
            request = json.loads(connection.makefile("rb").readline())
            result = sidecar.handle(request)
            connection.sendall(response_bytes({"ok": True, "result": result}))
        except Exception as exc:  # noqa: BLE001 - protocol boundary
            connection.sendall(
                response_bytes(
                    {
                        "ok": False,
                        "error": f"{type(exc).__name__}: {exc}",
                    }
                )
            )


def main() -> int:
    terminate_with_parent()
    args = parse_args()
    sidecar = Sidecar(args)
    args.socket.parent.mkdir(parents=True, exist_ok=True)
    args.socket.unlink(missing_ok=True)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(args.socket))
    os.chmod(args.socket, 0o600)
    server.listen(args.max_inflight)
    executor = concurrent.futures.ThreadPoolExecutor(
        max_workers=args.max_inflight,
        thread_name_prefix="awi-sidecar",
    )
    slots = threading.BoundedSemaphore(args.max_inflight)
    try:
        while True:
            slots.acquire()
            try:
                connection, _ = server.accept()
            except BaseException:
                slots.release()
                raise
            future = executor.submit(handle_connection, sidecar, connection)
            future.add_done_callback(lambda _future: slots.release())
    finally:
        server.close()
        executor.shutdown(wait=True, cancel_futures=True)
        args.socket.unlink(missing_ok=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
