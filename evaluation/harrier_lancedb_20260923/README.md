# Harrier CPU Embedding And LanceDB Exploration

Date: 2026-09-23

Status: exploration complete; production integration not enabled.

## Decision

Use **Harrier OSS v1 270M ONNX `ffn_skip`** for the first AWI Python sidecar
prototype and **LanceDB** as the vector store.

- `ffn_skip` preserved Recall@10 at 0.875, reached 2,641 tokens/s, and used
  2,150 MiB peak RSS on the test worker.
- GGUF Q8 is the fidelity fallback: its median document cosine against the
  official FP32 model was 0.99960 and its ranking metrics were effectively
  unchanged, but it was materially slower on this AMD EPYC CPU.
- The official FP32 model remains the quality reference and was already fast
  at 2,533 tokens/s. It is a reasonable fallback when an extra ~1 GiB RSS is
  acceptable.
- Do not use ONNX `per_channel` by default. Its higher throughput came with a
  clear retrieval-quality regression.
- Do not use the 0.6B model as the default CPU path. A partial full-corpus run
  processed only 400 of 1,139 inputs in roughly 8.5 minutes and was stopped.

This is not a release decision. The 16-query set is a development candidate
pending dual review, Recall@10 remains below AWI's 0.95 release gate, and the
fusion strategy still needs a clean, leakage-free evaluation.

## Model Contract

Harrier retrieval quality depends on preserving its asymmetric encoding
contract:

- query: prepend
  `Instruct: Given a software engineering query, retrieve the workspace code, configuration, dataset, or result that best answers it\nQuery: `
- document: no instruction
- pooling: last non-padding token
- output: L2-normalized vector
- chunking: at most 480 model tokens before special tokens, leaving room for
  BOS/EOS within the 512-token llama.cpp batch limit

Removing the query instruction kept Recall@10 at 0.875 on this small set but
reduced MRR from 0.6974 to 0.6313, nDCG@10 from 0.7396 to 0.6901, and median
query-vector cosine against FP32 to 0.8564.

## Evaluation Corpus

- 16 development queries: four each for code, SQL, experiment results, and
  dataset discovery
- 1,123 tokenizer-bounded chunks from 1,123 selected files
- 523,773 document tokens
- maximum encoded length: 482 tokens including special tokens
- 640-dimensional normalized embeddings
- AMD EPYC CPU worker, 16 inference threads

The candidate set is intentionally hard-negative-heavy and is not a frozen
release set. See `semantic.dev.json` for review state and evidence.

## Retrieval Results

| Backend | Recall@10 | MRR | nDCG@10 | tokens/s | Peak RSS MiB | Doc cosine P50 | Top-10 overlap |
|---|---:|---:|---:|---:|---:|---:|---:|
| Official FP32 | 0.8750 | 0.6958 | 0.7379 | 2,533 | 3,191 | 1.00000 | 1.0000 |
| GGUF Q8_0 | 0.8750 | 0.6974 | 0.7396 | 1,531 | 2,228 | 0.99960 | 0.9438 |
| GGUF Q5_K_M | 0.8750 | 0.6953 | 0.7377 | 1,126 | 2,198 | 0.99364 | 0.9063 |
| ONNX `ffn_skip` | 0.8750 | 0.6653 | 0.7156 | 2,641 | 2,150 | 0.99151 | 0.8938 |
| ONNX `per_channel` | 0.8125 | 0.5590 | 0.6199 | 3,239 | 1,947 | 0.96549 | 0.7313 |

The existing AWI lexical baseline scored Recall@10 0.5000, MRR 0.2750,
nDCG@10 0.3311, and P95 90.7 ms. Dense retrieval recovered substantially more
semantic matches, but all viable backends missed:

- `semantic-sql-002`: dense rank about 34; lexical rank 22
- `semantic-dataset-003`: dense rank about 1,107; lexical rank 2

A preliminary equal-weight RRF probe did not improve the recommended backend,
but its metrics are excluded from `semantic_results.json`: the lexical Top50
report contained `semantic.dev.json` at rank 49 for one query. The leaked
artifact did not enter any hybrid Top10, but the evaluation protocol requires
zero benchmark-artifact candidates. The raw candidate report is retained only
to make this invalidation auditable. Production fusion should use a clean
candidate run plus type-aware scopes and calibrated weights.

## LanceDB Results

| Corpus | Rows | DB size | IVF_PQ build | ANN P50/P95 | Filtered P50/P95 |
|---|---:|---:|---:|---:|---:|
| Real unique vectors | 1,123 | 3.82 MB | 0.30 s | 2.82 / 3.31 ms | 3.12 / 3.57 ms |
| Capacity probe | 100,000 | 269.6 MB | 7.64 s | 4.19 / 4.77 ms | 9.62 / 10.51 ms |

Both runs passed insert, update, delete, latest-version checkout, and historical
time-travel reads. The 100,000-row probe repeats the 1,123 source vectors, so it
validates storage amplification, lifecycle, and query latency only. It must not
be used to claim ANN recall quality.

The LanceDB query timings exclude query embedding time.

## Proposed AWI Architecture

1. Keep SQLite as the authoritative file/chunk catalog and Tantivy as the
   authoritative lexical retrieval lane.
2. Run Harrier ONNX in an optional Python sidecar. Embed structure-aware,
   tokenizer-bounded chunks and key every vector by stable `chunk_id` plus AWI
   catalog generation.
3. Store vectors and filterable metadata in a local mutable LanceDB generation.
   Never place the active writer on NFS.
4. Validate catalog/vector parity, seal the generation, and publish an immutable
   snapshot to NFS alongside the corresponding AWI generation.
5. Retrieve vector top 50 with root, kind, path-prefix, and generation filters.
   Fuse with Tantivy using field/type-aware weights and deterministic tie
   breaking.
6. Put the semantic lane behind a feature flag. On model, sidecar, generation,
   or LanceDB failure, return the current Tantivy result without delaying the
   normal query path.

## Release Gates

Before enabling semantic retrieval by default:

- dual-review and freeze a larger representative gold set;
- achieve Recall@10 >= 0.95 with no MRR/nDCG regression by query class;
- add stale-vector rejection, generation rollback, sidecar timeout, and
  unavailable-sidecar tests;
- benchmark end-to-end query embedding plus LanceDB retrieval, not vector search
  alone;
- tune field/type-aware fusion against the two observed long-tail failures;
- run the real-Agent A/B protocol with unchanged correctness and bounded
  response size.

## Artifacts

- `semantic.dev.json`: candidate queries and evidence
- `model_manifest.json`: pinned repositories, revisions, checksums, dimensions,
  and encoding contract
- `run_experiment.py`: corpus, embedding, evaluation, fusion, and LanceDB harness
- `lexical_baseline.json`: current AWI top-10 baseline
- `lexical_top50.json`: discarded candidate report retained for leakage audit
- `semantic_results.json`: valid dense-backend quality, fidelity, and throughput
- `lancedb_unique_results.json`: real-vector store benchmark
- `lancedb_results.json`: 100,000-row capacity benchmark
- `requirements.txt`: pinned Python environment
