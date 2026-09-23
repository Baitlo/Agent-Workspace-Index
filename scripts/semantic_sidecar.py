"""Persistent Harrier GGUF embedding and LanceDB sidecar for AWI."""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import math
import os
import shutil
import socket
import time
from pathlib import Path
from typing import Any

QUERY_INSTRUCTION = (
    "Instruct: Given a software engineering query, retrieve the workspace code, "
    "configuration, dataset, or result that best answers it\nQuery: "
)
ELIGIBLE_KINDS = {"source", "text", "semi_structured", "tabular"}


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--model", type=Path, required=True)
    parser.add_argument("--model-sha256")
    parser.add_argument("--threads", type=int, default=16)
    parser.add_argument("--batch-size", type=int, default=16)
    parser.add_argument("--embedding-workers", type=int, default=1)
    parser.add_argument("--max-tokens", type=int, default=480)
    parser.add_argument("--max-chunks-per-file", type=int, default=4)
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
        if self.threads < 1:
            raise ValueError("threads must be positive")
        if self.embedding_workers < 1:
            raise ValueError("embedding-workers must be positive")
        self.llama = Llama
        self.model = self.create_model(self.threads, 512)
        self.update_models: list[Any] | None = None
        self.tables: dict[str, Any] = {}

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
        self.tables.pop(str(database.resolve()), None)
        if database.exists():
            shutil.rmtree(database)
        return {"reset": True}

    def document_chunks(self, document: dict[str, Any]) -> list[dict[str, Any]]:
        if document["kind"] not in ELIGIBLE_KINDS:
            return []
        header = f"Path: {document['path']}\nFile type: {document['kind']}\n"
        chunks: list[dict[str, Any]] = []
        for section in chunk_text(document["content"]) or [""]:
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
                if len(chunks) >= self.max_chunks_per_file:
                    return chunks
        return chunks

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
        self.tables[key] = table
        return {
            "files": len({row["file_id"] for row in rows}),
            "chunks": len(rows),
            "dimension": dimension,
        }

    def open_table(self, database: Path) -> Any:
        key = str(database.resolve())
        if key not in self.tables:
            db = self.lancedb.connect(database)
            if "chunks" not in db.list_tables().tables:
                raise RuntimeError(f"semantic table is missing under {database}")
            self.tables[key] = db.open_table("chunks")
        return self.tables[key]

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
        already_sealed = (
            existing is not None
            and int(existing.get("generation", -1)) == generation
            and int(existing.get("files", -1)) == len(expected_pairs)
            and int(existing.get("rows", -1)) == row_count
            and existing.get("model_sha256") == self.model_sha256
            and existing.get("vector_index_type") == index_type
            and int(existing.get("vector_index_partitions", -1)) == partitions
        )
        if row_count >= 256 and not already_sealed:
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
        return payload

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
        metadata = json.loads(manifest.read_text(encoding="utf-8"))
        if int(metadata["generation"]) != generation:
            raise RuntimeError(
                f"semantic generation {metadata['generation']} != catalog {generation}"
            )
        if self.model_sha256 and metadata.get("model_sha256") != self.model_sha256:
            raise RuntimeError(
                "semantic manifest model checksum does not match runtime"
            )
        vector = self.model.embed([QUERY_INSTRUCTION + query], normalize=True)[0]
        predicates = [f"generation <= {generation}"]
        if roots:
            root_filters = [
                f"(path = {sql_string(root)} OR path LIKE {sql_string(root.rstrip('/') + '/%')})"
                for root in roots
            ]
            predicates.append("(" + " OR ".join(root_filters) + ")")
        eligible_kinds = sorted(set(kinds) & ELIGIBLE_KINDS) if kinds else []
        if kinds and not eligible_kinds:
            return {"candidates": []}
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
        rows = search.limit(max(limit * 8, limit)).to_list()
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
        return {"candidates": candidates}


def response_bytes(value: dict[str, Any]) -> bytes:
    return (
        json.dumps(value, ensure_ascii=False, separators=(",", ":")) + "\n"
    ).encode()


def main() -> int:
    args = parse_args()
    sidecar = Sidecar(args)
    args.socket.parent.mkdir(parents=True, exist_ok=True)
    args.socket.unlink(missing_ok=True)
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(args.socket))
    os.chmod(args.socket, 0o600)
    server.listen(16)
    try:
        while True:
            connection, _ = server.accept()
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
    finally:
        server.close()
        args.socket.unlink(missing_ok=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
