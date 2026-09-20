# Dual-review checklist

Reviewers work independently and record names only after resolving disagreements.

- [ ] Every query represents a real Qianchuan retrieval need, not a filename lookup disguised as a query.
- [ ] Every `relevant_paths` entry exists under one declared `corpus_roots` path.
- [ ] Code cases identify the implementation that performs the described behavior.
- [ ] SQL cases identify the authoritative query for the stated scene and data window.
- [ ] Experiment-result cases point to finalized summaries or auditable outputs, not transient logs.
- [ ] Dataset-query cases identify files whose profiled schema supports the stated fields.
- [ ] Each no-AWI `baseline_tool_calls` count is reproduced from an equivalent blind trace.
- [ ] Reviewer 1 labels all cases without seeing reviewer 2's labels.
- [ ] Reviewer 2 labels all cases without seeing reviewer 1's labels.
- [ ] Disagreements are adjudicated and documented before changing `review_status` to `approved`.
- [ ] The approved development set has two distinct names in `reviewers`.
- [ ] Recall@10 >= 0.95, P95 <= 200 ms, stale result rate = 0, and tool-call reduction >= 50%.
- [ ] No frozen split is created or evaluated before all checks above pass.
