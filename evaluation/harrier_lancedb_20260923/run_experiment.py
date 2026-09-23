#!/usr/bin/env python3
"""Reproducible CPU embedding and LanceDB exploration for AWI."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import resource
import shutil
import statistics
import sys
import time
from collections import defaultdict
from collections.abc import Iterable
from pathlib import Path
from typing import Any

import numpy as np

QUERY_INSTRUCTION = (
    "Instruct: Given a software engineering query, retrieve the workspace code, "
    "configuration, dataset, or result that best answers it\nQuery: "
)
ALLOWED_SUFFIXES = {
    ".c",
    ".cc",
    ".cpp",
    ".csv",
    ".go",
    ".h",
    ".hpp",
    ".java",
    ".js",
    ".json",
    ".jsonl",
    ".md",
    ".py",
    ".rs",
    ".sql",
    ".toml",
    ".ts",
    ".tsx",
    ".tsv",
    ".txt",
    ".yaml",
    ".yml",
}
EXCLUDED_DIRECTORIES = {
    ".cache",
    ".codex-work",
    ".git",
    ".hg",
    ".idea",
    ".ipynb_checkpoints",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".svn",
    ".venv",
    ".vscode",
    ".worktrees",
    "__pycache__",
    "node_modules",
    "target",
}
MODEL_PATHS = {
    "official_fp32": ("/mnt/bn/baiweikang/Models/harrier-oss-v1-270m"),
    "gguf_270m_q8": (
        "/mnt/bn/baiweikang/Models/harrier-oss-v1-270m-GGUF/"
        "harrier-oss-v1-270M-Q8_0.gguf"
    ),
    "gguf_270m_q5": (
        "/mnt/bn/baiweikang/Models/harrier-oss-v1-270m-GGUF/"
        "harrier-oss-v1-270M-Q5_K_M.gguf"
    ),
    "onnx_270m_per_channel": (
        "/mnt/bn/baiweikang/Models/harrier-oss-v1-270m-ONNX/onnx/model-per_channel.onnx"
    ),
    "onnx_270m_ffn_skip": (
        "/mnt/bn/baiweikang/Models/harrier-oss-v1-270m-ONNX/onnx/model-ffn_skip.onnx"
    ),
    "gguf_0_6b_q8": (
        "/mnt/bn/baiweikang/Models/harrier-oss-v1-0.6b-GGUF/"
        "harrier-oss-v1-0.6B-Q8_0.gguf"
    ),
    "gguf_0_6b_q5": (
        "/mnt/bn/baiweikang/Models/harrier-oss-v1-0.6b-GGUF/"
        "harrier-oss-v1-0.6B-Q5_K_M.gguf"
    ),
}
TOKENIZER_PATH = "/mnt/bn/baiweikang/Models/harrier-oss-v1-270m"
ONNX_TOKENIZER_PATH = "/mnt/bn/baiweikang/Models/harrier-oss-v1-270m-ONNX"


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        json.dumps(value, ensure_ascii=False, indent=2) + "\n",
        encoding="utf-8",
    )


def sha256_text(value: str) -> str:
    return hashlib.sha256(value.encode("utf-8")).hexdigest()


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        return 0.0
    ordered = sorted(values)
    index = min(len(ordered) - 1, math.ceil(fraction * len(ordered)) - 1)
    return ordered[max(index, 0)]


def mean(values: Iterable[float]) -> float:
    items = list(values)
    return statistics.fmean(items) if items else 0.0


def path_kind(path: Path) -> str:
    suffix = path.suffix.lower()
    if suffix in {
        ".py",
        ".rs",
        ".go",
        ".java",
        ".js",
        ".ts",
        ".tsx",
        ".c",
        ".cc",
        ".cpp",
        ".h",
        ".hpp",
    }:
        return "code"
    if suffix == ".sql":
        return "sql"
    if suffix in {".csv", ".json", ".jsonl", ".tsv", ".yaml", ".yml", ".toml"}:
        return "structured"
    return "text"


def discover_files(roots: list[Path]) -> list[tuple[Path, Path]]:
    discovered: dict[str, tuple[Path, Path]] = {}
    for root in roots:
        if not root.is_dir():
            continue
        for directory, names, files in os.walk(root, followlinks=False):
            names[:] = sorted(
                name
                for name in names
                if name not in EXCLUDED_DIRECTORIES
                and not Path(directory, name).is_symlink()
            )
            for name in sorted(files):
                path = Path(directory, name)
                if path.suffix.lower() not in ALLOWED_SUFFIXES or path.is_symlink():
                    continue
                discovered[str(path.resolve())] = (path.resolve(), root.resolve())
    return sorted(discovered.values(), key=lambda item: str(item[0]))


def bounded_file_bytes(path: Path, limit: int) -> tuple[bytes, bool]:
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    return data[:limit], len(data) > limit


def json_key_paths(value: Any, prefix: str = "", depth: int = 0) -> list[str]:
    if depth > 3:
        return []
    if isinstance(value, dict):
        output = []
        for key, child in list(value.items())[:80]:
            child_path = f"{prefix}.{key}" if prefix else str(key)
            output.append(child_path)
            output.extend(json_key_paths(child, child_path, depth + 1))
        return output
    if isinstance(value, list) and value:
        return json_key_paths(value[0], f"{prefix}[]", depth + 1)
    return []


def document_text(path: Path, byte_limit: int) -> tuple[str, bool]:
    data, truncated = bounded_file_bytes(path, byte_limit)
    if b"\x00" in data:
        return "", truncated
    text = data.decode("utf-8", errors="replace")
    if path.suffix.lower() == ".jsonl":
        first_line = next((line for line in text.splitlines() if line.strip()), "")
        try:
            row = json.loads(first_line)
            keys = ", ".join(json_key_paths(row))
            text = f"JSONL schema keys: {keys}\nFirst record:\n{first_line}"
        except json.JSONDecodeError:
            pass
    elif path.suffix.lower() == ".json":
        try:
            value = json.loads(text)
            keys = ", ".join(json_key_paths(value))
            text = f"JSON schema keys: {keys}\nContent:\n{text}"
        except json.JSONDecodeError:
            pass
    return text, truncated


def chunk_text(text: str, target_chars: int, overlap_lines: int) -> list[str]:
    lines = text.splitlines()
    chunks: list[str] = []
    current: list[str] = []
    current_chars = 0
    for line in lines:
        boundary = line.startswith(
            ("def ", "class ", "fn ", "pub fn ", "# ", "## ", "-- ")
        ) or line.rstrip().endswith(" AS (")
        if current and current_chars >= target_chars and boundary:
            chunks.append("\n".join(current).strip())
            current = current[-overlap_lines:]
            current_chars = sum(len(item) + 1 for item in current)
        current.append(line)
        current_chars += len(line) + 1
        if current_chars >= target_chars * 2:
            chunks.append("\n".join(current).strip())
            current = current[-overlap_lines:]
            current_chars = sum(len(item) + 1 for item in current)
    if current:
        chunks.append("\n".join(current).strip())
    return [chunk for chunk in chunks if chunk]


def truncate_to_tokens(text: str, tokenizer: Any, max_tokens: int) -> tuple[str, bool]:
    encoded = tokenizer(
        text,
        add_special_tokens=False,
        return_offsets_mapping=True,
        truncation=True,
        max_length=max_tokens,
    )
    offsets = encoded["offset_mapping"]
    if not offsets:
        return "", False
    truncated = len(tokenizer.encode(text, add_special_tokens=False)) > max_tokens
    return text[: offsets[-1][1]], truncated


def choose_files(
    gold: dict[str, Any],
    hard_negative_report: Path | None,
    max_files: int,
) -> tuple[list[tuple[Path, Path]], dict[str, str]]:
    roots = [Path(root).resolve() for root in gold["corpus_roots"]]
    discovered = discover_files(roots)
    root_by_path = {str(path): str(root) for path, root in discovered}
    required = {
        str(Path(path).resolve())
        for case in gold["cases"]
        for path in case["relevant_paths"]
    }
    hard_negatives: set[str] = set()
    if hard_negative_report:
        report = read_json(hard_negative_report)
        hard_negatives = {
            str(Path(path).resolve())
            for result in report.get("results", [])
            for path in result.get("returned_paths", [])
        }
    missing = sorted(path for path in required if path not in root_by_path)
    if missing:
        raise RuntimeError(f"relevant paths were not discovered: {missing}")
    priority = sorted(required | (hard_negatives & root_by_path.keys()))
    remaining = sorted(
        (path for path in root_by_path if path not in priority),
        key=lambda path: (sha256_text(path), path),
    )
    selected = (priority + remaining)[: max(max_files, len(priority))]
    roots_for_selected = {path: root_by_path[path] for path in selected}
    return [
        (Path(path), Path(roots_for_selected[path])) for path in selected
    ], roots_for_selected


def prepare_corpus(args: argparse.Namespace) -> None:
    from transformers import AutoTokenizer

    gold = read_json(args.gold)
    files, _ = choose_files(gold, args.hard_negative_report, args.max_files)
    tokenizer = AutoTokenizer.from_pretrained(TOKENIZER_PATH)
    rows = []
    skipped = []
    for path, root in files:
        try:
            text, truncated = document_text(path, args.max_bytes_per_file)
            if not text.strip():
                skipped.append({"path": str(path), "reason": "empty_or_binary"})
                continue
            content_hash = sha256_text(text)
            relative_path = str(path.relative_to(root))
            kind = path_kind(path)
            chunks = chunk_text(text, args.chunk_chars, args.overlap_lines)[
                : args.max_chunks_per_file
            ]
            for index, chunk in enumerate(chunks):
                prefix = (
                    f"Path: {relative_path}\n"
                    f"File type: {kind}\n"
                    f"Chunk: {index + 1}/{len(chunks)}\n"
                )
                embedded_text, token_truncated = truncate_to_tokens(
                    prefix + chunk, tokenizer, args.max_chunk_tokens
                )
                rows.append(
                    {
                        "chunk_id": sha256_text(f"{path}\0{index}\0{embedded_text}")[
                            :32
                        ],
                        "path": str(path),
                        "root": str(root),
                        "kind": kind,
                        "content_hash": content_hash,
                        "mtime_ns": path.stat().st_mtime_ns,
                        "source_truncated": truncated or token_truncated,
                        "text": embedded_text,
                    }
                )
        except (OSError, UnicodeError) as exc:
            skipped.append({"path": str(path), "reason": str(exc)})
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", encoding="utf-8") as stream:
        for row in rows:
            stream.write(json.dumps(row, ensure_ascii=False) + "\n")
    manifest = {
        "created_at_unix": time.time(),
        "gold": str(args.gold.resolve()),
        "selected_files": len(files),
        "indexed_files": len({row["path"] for row in rows}),
        "chunks": len(rows),
        "max_files": args.max_files,
        "max_bytes_per_file": args.max_bytes_per_file,
        "chunk_chars": args.chunk_chars,
        "max_chunk_tokens": args.max_chunk_tokens,
        "max_chunks_per_file": args.max_chunks_per_file,
        "skipped": skipped,
        "corpus_sha256": sha256_text(
            "\n".join(f"{row['chunk_id']}:{sha256_text(row['text'])}" for row in rows)
        ),
    }
    write_json(args.output.with_suffix(".manifest.json"), manifest)
    print(json.dumps(manifest, ensure_ascii=False, indent=2))


def load_corpus(path: Path) -> list[dict[str, Any]]:
    with path.open(encoding="utf-8") as stream:
        return [json.loads(line) for line in stream if line.strip()]


def l2_normalize(vectors: np.ndarray) -> np.ndarray:
    vectors = np.asarray(vectors, dtype=np.float32)
    norms = np.linalg.norm(vectors, axis=1, keepdims=True)
    return vectors / np.maximum(norms, np.finfo(np.float32).eps)


def batched(values: list[str], size: int) -> Iterable[list[str]]:
    for start in range(0, len(values), size):
        yield values[start : start + size]


def encode_official(
    texts: list[str],
    model_path: str,
    threads: int,
    batch_size: int,
) -> tuple[np.ndarray, float]:
    import torch
    from sentence_transformers import SentenceTransformer

    torch.set_num_threads(threads)
    started = time.perf_counter()
    model = SentenceTransformer(
        model_path,
        device="cpu",
        model_kwargs={"dtype": torch.float32},
    )
    model.max_seq_length = 512
    load_seconds = time.perf_counter() - started
    vectors = model.encode(
        texts,
        batch_size=batch_size,
        convert_to_numpy=True,
        normalize_embeddings=True,
        show_progress_bar=True,
    )
    return np.asarray(vectors, dtype=np.float32), load_seconds


def encode_gguf(
    texts: list[str],
    model_path: str,
    threads: int,
    batch_size: int,
) -> tuple[np.ndarray, float]:
    from llama_cpp import Llama

    started = time.perf_counter()
    model = Llama(
        model_path=model_path,
        embedding=True,
        n_ctx=512,
        n_batch=512,
        n_ubatch=512,
        n_threads=threads,
        n_threads_batch=threads,
        verbose=False,
    )
    load_seconds = time.perf_counter() - started
    output = []
    for index, batch in enumerate(batched(texts, batch_size), 1):
        output.extend(model.embed(batch, normalize=True))
        if index % 25 == 0:
            print(
                f"encoded {min(index * batch_size, len(texts))}/{len(texts)}",
                file=sys.stderr,
                flush=True,
            )
    return np.asarray(output, dtype=np.float32), load_seconds


def encode_onnx(
    texts: list[str],
    model_path: str,
    threads: int,
    batch_size: int,
) -> tuple[np.ndarray, float]:
    import onnxruntime as ort
    from transformers import AutoTokenizer

    tokenizer = AutoTokenizer.from_pretrained(ONNX_TOKENIZER_PATH)
    options = ort.SessionOptions()
    options.intra_op_num_threads = threads
    options.inter_op_num_threads = 1
    options.execution_mode = ort.ExecutionMode.ORT_SEQUENTIAL
    started = time.perf_counter()
    session = ort.InferenceSession(
        model_path,
        sess_options=options,
        providers=["CPUExecutionProvider"],
    )
    load_seconds = time.perf_counter() - started
    output = []
    for index, batch in enumerate(batched(texts, batch_size), 1):
        encoded = tokenizer(
            batch,
            padding=True,
            truncation=True,
            max_length=512,
            return_tensors="np",
        )
        vectors = session.run(
            ["sentence_embedding"],
            {
                "input_ids": encoded["input_ids"],
                "attention_mask": encoded["attention_mask"],
            },
        )[0]
        output.append(l2_normalize(vectors))
        if index % 25 == 0:
            print(
                f"encoded {min(index * batch_size, len(texts))}/{len(texts)}",
                file=sys.stderr,
                flush=True,
            )
    return np.concatenate(output), load_seconds


def count_tokens(texts: list[str]) -> int:
    from transformers import AutoTokenizer

    tokenizer = AutoTokenizer.from_pretrained(TOKENIZER_PATH)
    return sum(
        len(tokenizer.encode(text, truncation=True, max_length=512)) for text in texts
    )


def encode_backend(
    backend: str,
    texts: list[str],
    threads: int,
    batch_size: int,
) -> tuple[np.ndarray, float]:
    model_path = MODEL_PATHS[backend]
    if backend == "official_fp32":
        return encode_official(texts, model_path, threads, batch_size)
    if backend.startswith("onnx_"):
        return encode_onnx(texts, model_path, threads, batch_size)
    return encode_gguf(texts, model_path, threads, batch_size)


def embed(args: argparse.Namespace) -> None:
    corpus = load_corpus(args.corpus)
    gold = read_json(args.gold)
    documents = [row["text"] for row in corpus]
    raw_queries = [case["query"] for case in gold["cases"]]
    queries = (
        raw_queries
        if args.no_query_instruction
        else [QUERY_INSTRUCTION + query for query in raw_queries]
    )
    reused = None
    if args.reuse_document_embeddings:
        reused = np.load(args.reuse_document_embeddings)
        expected_ids = np.asarray([row["chunk_id"] for row in corpus])
        if not np.array_equal(expected_ids, reused["chunk_ids"]):
            raise RuntimeError("reused document embeddings do not match the corpus")
        all_texts = queries
    else:
        all_texts = documents + queries
    token_count = count_tokens(all_texts)
    started = time.perf_counter()
    vectors, load_seconds = encode_backend(
        args.backend,
        all_texts,
        args.threads,
        args.batch_size,
    )
    encode_seconds = time.perf_counter() - started - load_seconds
    if reused is None:
        document_vectors = vectors[: len(documents)]
        query_vectors = vectors[len(documents) :]
        documents_encoded = len(documents)
    else:
        document_vectors = reused["document_vectors"]
        query_vectors = vectors
        documents_encoded = 0
    output = args.output
    output.parent.mkdir(parents=True, exist_ok=True)
    np.savez(
        output,
        chunk_ids=np.asarray([row["chunk_id"] for row in corpus]),
        paths=np.asarray([row["path"] for row in corpus]),
        document_vectors=document_vectors,
        query_ids=np.asarray([case["id"] for case in gold["cases"]]),
        query_vectors=query_vectors,
    )
    metrics = {
        "backend": args.backend,
        "model_path": MODEL_PATHS[args.backend],
        "threads": args.threads,
        "batch_size": args.batch_size,
        "query_instruction": not args.no_query_instruction,
        "documents": len(documents),
        "documents_encoded": documents_encoded,
        "document_embeddings_reused_from": (
            str(args.reuse_document_embeddings) if reused is not None else None
        ),
        "queries": len(queries),
        "embedding_dimension": int(document_vectors.shape[1]),
        "tokens": token_count,
        "load_seconds": load_seconds,
        "encode_seconds": encode_seconds,
        "texts_per_second": len(all_texts) / encode_seconds,
        "tokens_per_second": token_count / encode_seconds,
        "peak_rss_mib": resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024,
        "output_bytes": output.stat().st_size,
    }
    write_json(output.with_suffix(".metrics.json"), metrics)
    print(json.dumps(metrics, ensure_ascii=False, indent=2))


def subset_embeddings(args: argparse.Namespace) -> None:
    corpus = load_corpus(args.corpus)
    source = np.load(args.source)
    source_index = {
        str(chunk_id): index for index, chunk_id in enumerate(source["chunk_ids"])
    }
    target_ids = [row["chunk_id"] for row in corpus]
    missing = [chunk_id for chunk_id in target_ids if chunk_id not in source_index]
    if missing:
        raise RuntimeError(f"source embeddings are missing chunk IDs: {missing[:5]}")
    indices = np.asarray([source_index[chunk_id] for chunk_id in target_ids])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    np.savez(
        args.output,
        chunk_ids=np.asarray(target_ids),
        paths=np.asarray([row["path"] for row in corpus]),
        document_vectors=source["document_vectors"][indices],
        query_ids=source["query_ids"],
        query_vectors=source["query_vectors"],
    )
    source_metrics = read_json(args.source.with_suffix(".metrics.json"))
    metrics = {
        **source_metrics,
        "documents": len(corpus),
        "documents_encoded": 0,
        "derived_from": str(args.source),
        "profile_scope": "source full-corpus run",
        "output_bytes": args.output.stat().st_size,
    }
    write_json(args.output.with_suffix(".metrics.json"), metrics)
    print(json.dumps(metrics, ensure_ascii=False, indent=2))


def ranked_paths(
    document_vectors: np.ndarray,
    paths: np.ndarray,
    query_vector: np.ndarray,
    limit: int,
) -> tuple[list[str], float]:
    started = time.perf_counter()
    scores = document_vectors @ query_vector
    order = np.argsort(-scores)
    output = []
    seen = set()
    for index in order:
        path = str(paths[index])
        if path in seen:
            continue
        seen.add(path)
        output.append(path)
        if len(output) >= limit:
            break
    return output, (time.perf_counter() - started) * 1000


def case_metrics(
    case: dict[str, Any],
    returned_paths: list[str],
    latency_ms: float,
) -> dict[str, Any]:
    relevant = {str(Path(path).resolve()) for path in case["relevant_paths"]}
    ranks = [
        rank
        for rank, path in enumerate(returned_paths, 1)
        if str(Path(path).resolve()) in relevant
    ]
    found = len({returned_paths[rank - 1] for rank in ranks})
    recall = found / len(relevant)
    reciprocal_rank = 1.0 / ranks[0] if ranks else 0.0
    dcg = sum(1.0 / math.log2(rank + 1) for rank in ranks)
    ideal = sum(
        1.0 / math.log2(rank + 1) for rank in range(1, min(len(relevant), 10) + 1)
    )
    return {
        "id": case["id"],
        "category": case["category"],
        "recall_at_10": recall,
        "reciprocal_rank": reciprocal_rank,
        "ndcg_at_10": dcg / ideal if ideal else 0.0,
        "latency_ms": latency_ms,
        "returned_paths": returned_paths,
    }


def aggregate_case_metrics(results: list[dict[str, Any]]) -> dict[str, Any]:
    latencies = [result["latency_ms"] for result in results]
    categories: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for result in results:
        categories[result["category"]].append(result)
    return {
        "cases": len(results),
        "recall_at_10": mean(item["recall_at_10"] for item in results),
        "mrr": mean(item["reciprocal_rank"] for item in results),
        "ndcg_at_10": mean(item["ndcg_at_10"] for item in results),
        "p50_query_ms": percentile(latencies, 0.50),
        "p95_query_ms": percentile(latencies, 0.95),
        "categories": {
            category: {
                "cases": len(items),
                "recall_at_10": mean(item["recall_at_10"] for item in items),
                "mrr": mean(item["reciprocal_rank"] for item in items),
                "ndcg_at_10": mean(item["ndcg_at_10"] for item in items),
            }
            for category, items in sorted(categories.items())
        },
    }


def cosine_summary(left: np.ndarray, right: np.ndarray) -> dict[str, float]:
    left = l2_normalize(left)
    right = l2_normalize(right)
    values = np.sum(left * right, axis=1)
    return {
        "min": float(values.min()),
        "p50": float(np.median(values)),
        "mean": float(values.mean()),
        "p95": float(np.percentile(values, 95)),
    }


def rrf_rank(
    lexical_paths: list[str],
    vector_paths: list[str],
    limit: int,
    rrf_k: int = 60,
) -> list[str]:
    scores: dict[str, float] = defaultdict(float)
    for paths in (lexical_paths, vector_paths):
        for rank, path in enumerate(paths, 1):
            scores[str(Path(path).resolve())] += 1.0 / (rrf_k + rank)
    return [
        path
        for path, _ in sorted(scores.items(), key=lambda item: (-item[1], item[0]))[
            :limit
        ]
    ]


def reject_evaluation_artifact_leakage(
    gold_path: Path,
    lexical_results: list[dict[str, Any]],
) -> None:
    artifact_root = gold_path.resolve().parent
    leaked = sorted(
        {
            str(Path(path).resolve())
            for result in lexical_results
            for path in result["returned_paths"]
            if Path(path).resolve().is_relative_to(artifact_root)
        }
    )
    if leaked:
        raise RuntimeError(
            "lexical candidates include evaluation artifacts under "
            f"{artifact_root}: {leaked}"
        )


def evaluate(args: argparse.Namespace) -> None:
    gold = read_json(args.gold)
    files = sorted(args.embeddings_dir.glob("*.npz"))
    if not files:
        raise RuntimeError(f"no embedding files under {args.embeddings_dir}")
    lexical_by_id = {}
    if args.lexical_report:
        lexical = read_json(args.lexical_report)
        reject_evaluation_artifact_leakage(args.gold, lexical["results"])
        lexical_by_id = {
            result["id"]: result["returned_paths"] for result in lexical["results"]
        }
    loaded = {}
    for path in files:
        data = np.load(path)
        loaded[path.stem] = {
            "path": path,
            "chunk_ids": data["chunk_ids"],
            "paths": data["paths"],
            "document_vectors": data["document_vectors"],
            "query_ids": data["query_ids"],
            "query_vectors": data["query_vectors"],
        }
    reference = loaded.get(args.reference)
    report = {"reference": args.reference, "backends": {}}
    reference_top10: dict[str, list[str]] = {}
    for backend, data in loaded.items():
        results = []
        top10_by_id = {}
        for case, query_id, query_vector in zip(
            gold["cases"], data["query_ids"], data["query_vectors"], strict=True
        ):
            if str(query_id) != case["id"]:
                raise RuntimeError(f"query order mismatch for {backend}")
            paths, latency_ms = ranked_paths(
                data["document_vectors"], data["paths"], query_vector, 10
            )
            top10_by_id[case["id"]] = paths
            results.append(case_metrics(case, paths, latency_ms))
        metrics = aggregate_case_metrics(results)
        metrics["results"] = results
        metrics["runtime"] = read_json(data["path"].with_suffix(".metrics.json"))
        if (
            reference
            and data["document_vectors"].shape == reference["document_vectors"].shape
        ):
            if not np.array_equal(data["chunk_ids"], reference["chunk_ids"]):
                raise RuntimeError(f"corpus mismatch for {backend}")
            metrics["document_cosine_vs_reference"] = cosine_summary(
                data["document_vectors"], reference["document_vectors"]
            )
            metrics["query_cosine_vs_reference"] = cosine_summary(
                data["query_vectors"], reference["query_vectors"]
            )
        report["backends"][backend] = metrics
        if backend == args.reference:
            reference_top10 = top10_by_id
    if reference_top10:
        for backend, metrics in report["backends"].items():
            overlaps = []
            for result in metrics["results"]:
                baseline = set(reference_top10[result["id"]])
                overlaps.append(
                    len(baseline & set(result["returned_paths"])) / len(baseline)
                )
            metrics["top10_overlap_vs_reference"] = mean(overlaps)
    if lexical_by_id:
        hybrid = {}
        for backend, metrics in report["backends"].items():
            data = loaded[backend]
            results = []
            for case, vector_result, query_vector in zip(
                gold["cases"],
                metrics["results"],
                data["query_vectors"],
                strict=True,
            ):
                vector_paths, _ = ranked_paths(
                    data["document_vectors"],
                    data["paths"],
                    query_vector,
                    args.fusion_depth,
                )
                paths = rrf_rank(
                    lexical_by_id.get(case["id"], []),
                    vector_paths,
                    10,
                )
                results.append(case_metrics(case, paths, 0.0))
            hybrid[backend] = {
                **aggregate_case_metrics(results),
                "results": results,
            }
        report["hybrid_rrf"] = hybrid
    write_json(args.output, report)
    print(json.dumps(report, ensure_ascii=False, indent=2))


def arrow_batch(
    corpus: list[dict[str, Any]],
    vectors: np.ndarray,
    start: int,
    end: int,
    generation: int,
) -> Any:
    import pyarrow as pa

    source_count = len(corpus)
    indices = np.arange(start, end) % source_count
    selected_vectors = np.asarray(vectors[indices], dtype=np.float32)
    dimension = selected_vectors.shape[1]
    flat_vectors = pa.array(selected_vectors.reshape(-1), type=pa.float32())
    vector_array = pa.FixedSizeListArray.from_arrays(flat_vectors, dimension)
    return pa.table(
        {
            "chunk_id": [
                f"{corpus[index]['chunk_id']}#replica-{row // source_count}"
                for row, index in zip(range(start, end), indices, strict=True)
            ],
            "path": [corpus[index]["path"] for index in indices],
            "root": [corpus[index]["root"] for index in indices],
            "kind": [corpus[index]["kind"] for index in indices],
            "content_hash": [corpus[index]["content_hash"] for index in indices],
            "mtime_ns": pa.array(
                [corpus[index]["mtime_ns"] for index in indices], type=pa.int64()
            ),
            "generation": pa.array([generation] * len(indices), type=pa.int64()),
            "vector": vector_array,
        }
    )


def directory_size(path: Path) -> int:
    return sum(
        item.stat().st_size
        for item in path.rglob("*")
        if item.is_file() and not item.is_symlink()
    )


def lancedb_benchmark(args: argparse.Namespace) -> None:
    import lancedb
    import pyarrow as pa

    if args.database.exists():
        if not args.overwrite or not str(args.database.resolve()).startswith("/tmp/"):
            raise RuntimeError(
                "refusing to replace database without --overwrite under /tmp"
            )
        shutil.rmtree(args.database)
    corpus = load_corpus(args.corpus)
    data = np.load(args.embeddings)
    if not np.array_equal(
        np.asarray([row["chunk_id"] for row in corpus]), data["chunk_ids"]
    ):
        raise RuntimeError("corpus and embedding chunk IDs differ")
    vectors = np.asarray(data["document_vectors"], dtype=np.float32)
    db = lancedb.connect(args.database)
    table = db.create_table(
        "chunks",
        data=arrow_batch(
            corpus,
            vectors,
            0,
            min(args.batch_rows, args.target_rows),
            generation=1,
        ),
        mode="overwrite",
    )
    inserted = min(args.batch_rows, args.target_rows)
    started = time.perf_counter()
    while inserted < args.target_rows:
        end = min(inserted + args.batch_rows, args.target_rows)
        table.add(
            arrow_batch(corpus, vectors, inserted, end, generation=1),
            mode="append",
        )
        inserted = end
    insert_seconds = time.perf_counter() - started
    pre_index_version = table.version
    started = time.perf_counter()
    partitions = min(256, max(16, args.target_rows // 1024))
    sub_vectors = 80 if vectors.shape[1] == 640 else 128
    table.create_index(
        metric="cosine",
        vector_column_name="vector",
        index_type="IVF_PQ",
        num_partitions=partitions,
        num_sub_vectors=sub_vectors,
        replace=True,
    )
    index_seconds = time.perf_counter() - started
    query_latencies = []
    filtered_latencies = []
    query_vectors = np.asarray(data["query_vectors"], dtype=np.float32)
    for _ in range(args.query_repetitions):
        for vector in query_vectors:
            started = time.perf_counter()
            (
                table.search(vector, vector_column_name="vector")
                .metric("cosine")
                .nprobes(min(16, partitions))
                .limit(10)
                .to_list()
            )
            query_latencies.append((time.perf_counter() - started) * 1000)
            started = time.perf_counter()
            (
                table.search(vector, vector_column_name="vector")
                .metric("cosine")
                .where("kind = 'code'", prefilter=True)
                .nprobes(min(16, partitions))
                .limit(10)
                .to_list()
            )
            filtered_latencies.append((time.perf_counter() - started) * 1000)
    before_mutation = table.version
    sentinel = arrow_batch(corpus, vectors, 0, 1, generation=99)
    sentinel_id = str(sentinel["chunk_id"][0].as_py()) + "-sentinel"
    sentinel = sentinel.set_column(
        0, "chunk_id", pa.array([sentinel_id], type=pa.string())
    )
    table.add(sentinel)
    inserted_version = table.version
    inserted_count = table.count_rows(f"chunk_id = '{sentinel_id}'")
    table.update(
        where=f"chunk_id = '{sentinel_id}'",
        values={"generation": 100},
    )
    updated_count = table.count_rows(f"chunk_id = '{sentinel_id}' AND generation = 100")
    table.delete(f"chunk_id = '{sentinel_id}'")
    deleted_count = table.count_rows(f"chunk_id = '{sentinel_id}'")
    latest_version = table.version
    latest_count = table.count_rows()
    table.checkout(before_mutation)
    historical_count = table.count_rows()
    table.checkout_latest()
    report = {
        "database": str(args.database),
        "source_embeddings": str(args.embeddings),
        "rows": latest_count,
        "dimension": int(vectors.shape[1]),
        "database_bytes": directory_size(args.database),
        "bytes_per_row": directory_size(args.database) / max(latest_count, 1),
        "insert_seconds_excluding_first_batch": insert_seconds,
        "rows_per_second": (
            (args.target_rows - min(args.batch_rows, args.target_rows))
            / max(insert_seconds, 1e-9)
        ),
        "index": {
            "type": "IVF_PQ",
            "metric": "cosine",
            "partitions": partitions,
            "sub_vectors": sub_vectors,
            "build_seconds": index_seconds,
            "indices": [str(index) for index in table.list_indices()],
        },
        "query_ms": {
            "samples": len(query_latencies),
            "p50": percentile(query_latencies, 0.50),
            "p95": percentile(query_latencies, 0.95),
        },
        "filtered_query_ms": {
            "filter": "kind = 'code'",
            "samples": len(filtered_latencies),
            "p50": percentile(filtered_latencies, 0.50),
            "p95": percentile(filtered_latencies, 0.95),
        },
        "lifecycle": {
            "pre_index_version": pre_index_version,
            "before_mutation_version": before_mutation,
            "inserted_version": inserted_version,
            "latest_version": latest_version,
            "inserted_count": inserted_count,
            "updated_count": updated_count,
            "deleted_count": deleted_count,
            "latest_count": latest_count,
            "historical_count": historical_count,
            "time_travel_read_passed": historical_count == args.target_rows,
            "insert_update_delete_passed": (
                inserted_count == 1 and updated_count == 1 and deleted_count == 0
            ),
        },
    }
    write_json(args.output, report)
    print(json.dumps(report, ensure_ascii=False, indent=2))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    prepare = subparsers.add_parser("prepare")
    prepare.add_argument("--gold", type=Path, required=True)
    prepare.add_argument("--hard-negative-report", type=Path)
    prepare.add_argument("--output", type=Path, required=True)
    prepare.add_argument("--max-files", type=int, default=1500)
    prepare.add_argument("--max-bytes-per-file", type=int, default=65536)
    prepare.add_argument("--chunk-chars", type=int, default=1800)
    prepare.add_argument("--max-chunk-tokens", type=int, default=480)
    prepare.add_argument("--overlap-lines", type=int, default=3)
    prepare.add_argument("--max-chunks-per-file", type=int, default=4)
    prepare.set_defaults(handler=prepare_corpus)

    embedding = subparsers.add_parser("embed")
    embedding.add_argument("--gold", type=Path, required=True)
    embedding.add_argument("--corpus", type=Path, required=True)
    embedding.add_argument("--backend", choices=sorted(MODEL_PATHS), required=True)
    embedding.add_argument("--threads", type=int, default=16)
    embedding.add_argument("--batch-size", type=int, default=16)
    embedding.add_argument("--no-query-instruction", action="store_true")
    embedding.add_argument("--reuse-document-embeddings", type=Path)
    embedding.add_argument("--output", type=Path, required=True)
    embedding.set_defaults(handler=embed)

    subset = subparsers.add_parser("subset")
    subset.add_argument("--corpus", type=Path, required=True)
    subset.add_argument("--source", type=Path, required=True)
    subset.add_argument("--output", type=Path, required=True)
    subset.set_defaults(handler=subset_embeddings)

    evaluation = subparsers.add_parser("evaluate")
    evaluation.add_argument("--gold", type=Path, required=True)
    evaluation.add_argument("--embeddings-dir", type=Path, required=True)
    evaluation.add_argument("--reference", default="official_fp32")
    evaluation.add_argument("--lexical-report", type=Path)
    evaluation.add_argument("--fusion-depth", type=int, default=50)
    evaluation.add_argument("--output", type=Path, required=True)
    evaluation.set_defaults(handler=evaluate)

    lance = subparsers.add_parser("lancedb")
    lance.add_argument("--corpus", type=Path, required=True)
    lance.add_argument("--embeddings", type=Path, required=True)
    lance.add_argument("--database", type=Path, required=True)
    lance.add_argument("--target-rows", type=int, default=100000)
    lance.add_argument("--batch-rows", type=int, default=10000)
    lance.add_argument("--query-repetitions", type=int, default=10)
    lance.add_argument("--overwrite", action="store_true")
    lance.add_argument("--output", type=Path, required=True)
    lance.set_defaults(handler=lancedb_benchmark)

    return parser.parse_args()


def main() -> None:
    args = parse_args()
    args.handler(args)


if __name__ == "__main__":
    main()
