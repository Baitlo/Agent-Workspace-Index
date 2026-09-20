use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::extract::mtime_ns;
use crate::{SearchHit, WorkspaceIndex};

const REQUIRED_RECALL_AT_10: f64 = 0.95;
const MAX_P95_MS: f64 = 200.0;
const MAX_STALE_RATE: f64 = 0.0;
const REQUIRED_TOOL_CALL_REDUCTION: f64 = 0.50;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalGoldSet {
    pub name: String,
    pub asset_family: String,
    pub split: String,
    pub review_status: String,
    #[serde(default)]
    pub reviewers: Vec<String>,
    #[serde(default)]
    pub corpus_roots: Vec<String>,
    pub cases: Vec<RetrievalCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalCase {
    pub id: String,
    pub category: String,
    pub query: String,
    pub relevant_paths: Vec<String>,
    pub baseline_tool_calls: u64,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalCaseResult {
    pub id: String,
    pub category: String,
    pub recall_at_10: f64,
    pub reciprocal_rank: f64,
    pub ndcg_at_10: f64,
    pub latency_ms: f64,
    pub stale_hits: usize,
    pub returned_paths: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RetrievalCategoryMetrics {
    pub cases: usize,
    pub recall_at_10: f64,
    pub mrr: f64,
    pub ndcg_at_10: f64,
    pub p95_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalGate {
    pub review_approved: bool,
    pub recall_at_10_passed: bool,
    pub p95_passed: bool,
    pub stale_rate_passed: bool,
    pub tool_call_reduction_passed: bool,
    pub release_passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalReport {
    pub gold_name: String,
    pub asset_family: String,
    pub split: String,
    pub review_status: String,
    pub cases: usize,
    pub recall_at_10: f64,
    pub mrr: f64,
    pub ndcg_at_10: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub stale_result_rate: f64,
    pub baseline_tool_calls: u64,
    pub awi_tool_calls: u64,
    pub tool_call_reduction: f64,
    pub categories: BTreeMap<String, RetrievalCategoryMetrics>,
    pub gate: RetrievalGate,
    pub results: Vec<RetrievalCaseResult>,
}

pub fn load_gold(path: &Path, allow_draft: bool) -> Result<RetrievalGoldSet> {
    let gold: RetrievalGoldSet = serde_json::from_slice(
        &fs::read(path).with_context(|| format!("read retrieval gold {}", path.display()))?,
    )
    .with_context(|| format!("decode retrieval gold {}", path.display()))?;
    if gold.cases.is_empty() {
        anyhow::bail!("retrieval gold contains no cases");
    }
    if gold.split.eq_ignore_ascii_case("frozen") {
        anyhow::bail!("frozen retrieval sets cannot be consumed by this command");
    }
    if gold.review_status != "approved" && !allow_draft {
        anyhow::bail!(
            "retrieval set review_status={} is not approved; pass --allow-draft only for development",
            gold.review_status
        );
    }
    if gold.review_status == "approved" && distinct_reviewers(&gold.reviewers) < 2 {
        anyhow::bail!("approved retrieval sets require two distinct non-empty reviewers");
    }
    for case in &gold.cases {
        if case.relevant_paths.is_empty() {
            anyhow::bail!("retrieval case {} has no relevant paths", case.id);
        }
        if case.baseline_tool_calls == 0 {
            anyhow::bail!("retrieval case {} has zero baseline_tool_calls", case.id);
        }
    }
    Ok(gold)
}

pub fn evaluate(index: &WorkspaceIndex, gold: &RetrievalGoldSet) -> Result<RetrievalReport> {
    let mut results = Vec::with_capacity(gold.cases.len());
    for case in &gold.cases {
        let started = Instant::now();
        let hits = index.search(&case.query, 10)?;
        let latency_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let relevant_ranks = hits
            .iter()
            .enumerate()
            .filter_map(|(rank, hit)| {
                case.relevant_paths
                    .iter()
                    .any(|expected| path_matches(&hit.path, expected))
                    .then_some(rank + 1)
            })
            .collect::<Vec<_>>();
        let relevant_found = case
            .relevant_paths
            .iter()
            .filter(|expected| {
                hits.iter()
                    .any(|hit| path_matches(&hit.path, expected.as_str()))
            })
            .count();
        let recall_at_10 = relevant_found as f64 / case.relevant_paths.len() as f64;
        let reciprocal_rank = relevant_ranks
            .first()
            .map_or(0.0, |rank| 1.0 / *rank as f64);
        let ndcg_at_10 = ndcg_at_10(&relevant_ranks, case.relevant_paths.len());
        let stale_hits = hits.iter().filter(|hit| hit_is_stale(hit)).count();
        results.push(RetrievalCaseResult {
            id: case.id.clone(),
            category: case.category.clone(),
            recall_at_10,
            reciprocal_rank,
            ndcg_at_10,
            latency_ms,
            stale_hits,
            returned_paths: hits.into_iter().map(|hit| hit.path).collect(),
        });
    }

    let recall_at_10 = mean(results.iter().map(|result| result.recall_at_10));
    let mrr = mean(results.iter().map(|result| result.reciprocal_rank));
    let ndcg_at_10 = mean(results.iter().map(|result| result.ndcg_at_10));
    let latencies = results
        .iter()
        .map(|result| result.latency_ms)
        .collect::<Vec<_>>();
    let stale_hits = results
        .iter()
        .map(|result| result.stale_hits)
        .sum::<usize>();
    let returned_hits = results
        .iter()
        .map(|result| result.returned_paths.len())
        .sum::<usize>();
    let stale_result_rate = if returned_hits == 0 {
        0.0
    } else {
        stale_hits as f64 / returned_hits as f64
    };
    let baseline_tool_calls = gold
        .cases
        .iter()
        .map(|case| case.baseline_tool_calls)
        .sum::<u64>();
    let awi_tool_calls = gold.cases.len() as u64;
    let tool_call_reduction = 1.0 - awi_tool_calls as f64 / baseline_tool_calls as f64;
    let categories = category_metrics(&results);
    let p50_ms = percentile(&latencies, 0.50);
    let p95_ms = percentile(&latencies, 0.95);
    let gate = RetrievalGate {
        review_approved: gold.review_status == "approved"
            && distinct_reviewers(&gold.reviewers) >= 2,
        recall_at_10_passed: recall_at_10 >= REQUIRED_RECALL_AT_10,
        p95_passed: p95_ms <= MAX_P95_MS,
        stale_rate_passed: stale_result_rate <= MAX_STALE_RATE,
        tool_call_reduction_passed: tool_call_reduction >= REQUIRED_TOOL_CALL_REDUCTION,
        release_passed: false,
    };
    let gate = RetrievalGate {
        release_passed: gate.review_approved
            && gate.recall_at_10_passed
            && gate.p95_passed
            && gate.stale_rate_passed
            && gate.tool_call_reduction_passed,
        ..gate
    };

    Ok(RetrievalReport {
        gold_name: gold.name.clone(),
        asset_family: gold.asset_family.clone(),
        split: gold.split.clone(),
        review_status: gold.review_status.clone(),
        cases: gold.cases.len(),
        recall_at_10,
        mrr,
        ndcg_at_10,
        p50_ms,
        p95_ms,
        stale_result_rate,
        baseline_tool_calls,
        awi_tool_calls,
        tool_call_reduction,
        categories,
        gate,
        results,
    })
}

fn category_metrics(results: &[RetrievalCaseResult]) -> BTreeMap<String, RetrievalCategoryMetrics> {
    let mut grouped = BTreeMap::<String, Vec<&RetrievalCaseResult>>::new();
    for result in results {
        grouped
            .entry(result.category.clone())
            .or_default()
            .push(result);
    }
    grouped
        .into_iter()
        .map(|(category, values)| {
            let latencies = values
                .iter()
                .map(|value| value.latency_ms)
                .collect::<Vec<_>>();
            (
                category,
                RetrievalCategoryMetrics {
                    cases: values.len(),
                    recall_at_10: mean(values.iter().map(|value| value.recall_at_10)),
                    mrr: mean(values.iter().map(|value| value.reciprocal_rank)),
                    ndcg_at_10: mean(values.iter().map(|value| value.ndcg_at_10)),
                    p95_ms: percentile(&latencies, 0.95),
                },
            )
        })
        .collect()
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let values = values.collect::<Vec<_>>();
    if values.is_empty() {
        0.0
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}

fn distinct_reviewers(reviewers: &[String]) -> usize {
    reviewers
        .iter()
        .map(|reviewer| reviewer.trim())
        .filter(|reviewer| !reviewer.is_empty())
        .collect::<HashSet<_>>()
        .len()
}

fn ndcg_at_10(relevant_ranks: &[usize], relevant_count: usize) -> f64 {
    let dcg = relevant_ranks
        .iter()
        .filter(|rank| **rank <= 10)
        .map(|rank| 1.0 / (*rank as f64 + 1.0).log2())
        .sum::<f64>();
    let ideal = (1..=relevant_count.min(10))
        .map(|rank| 1.0 / (rank as f64 + 1.0).log2())
        .sum::<f64>();
    if ideal == 0.0 { 0.0 } else { dcg / ideal }
}

fn percentile(values: &[f64], quantile: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let rank = ((values.len() - 1) as f64 * quantile).ceil() as usize;
    values[rank]
}

fn path_matches(actual: &str, expected: &str) -> bool {
    actual == expected
        || actual
            .strip_suffix(expected)
            .is_some_and(|prefix| prefix.is_empty() || prefix.ends_with('/'))
}

fn hit_is_stale(hit: &SearchHit) -> bool {
    let Ok(metadata) = fs::metadata(&hit.path) else {
        return true;
    };
    metadata.len() != hit.size_bytes || mtime_ns(&metadata) != hit.mtime_ns
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::IndexOptions;

    use super::*;

    #[test]
    fn percentile_uses_nearest_rank_ceiling() {
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.50), 3.0);
        assert_eq!(percentile(&[1.0, 2.0, 3.0, 4.0], 0.95), 4.0);
    }

    #[test]
    fn ndcg_rewards_earlier_complete_retrieval() {
        assert_eq!(ndcg_at_10(&[1], 1), 1.0);
        assert!(ndcg_at_10(&[1], 2) < 1.0);
        assert_eq!(ndcg_at_10(&[1, 2], 2), 1.0);
    }

    #[test]
    fn suffix_paths_match_only_on_component_boundaries() {
        assert!(path_matches(
            "/workspace/Qianchuan102_data_process/build.py",
            "Qianchuan102_data_process/build.py"
        ));
        assert!(!path_matches(
            "/workspace/notQianchuan102_data_process/build.py",
            "Qianchuan102_data_process/build.py"
        ));
    }

    #[test]
    fn refuses_unapproved_and_frozen_gold_sets() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("gold.json");
        let mut gold = fixture_gold("candidate_pending_dual_review", "dev");
        fs::write(&path, serde_json::to_vec(&gold).unwrap()).unwrap();
        assert!(load_gold(&path, false).is_err());
        assert!(load_gold(&path, true).is_ok());

        gold.split = "frozen".to_owned();
        gold.review_status = "approved".to_owned();
        fs::write(&path, serde_json::to_vec(&gold).unwrap()).unwrap();
        assert!(load_gold(&path, true).is_err());

        gold.split = "dev".to_owned();
        fs::write(&path, serde_json::to_vec(&gold).unwrap()).unwrap();
        assert!(load_gold(&path, true).is_err());
        gold.reviewers = vec!["reviewer-a".to_owned(), "reviewer-b".to_owned()];
        fs::write(&path, serde_json::to_vec(&gold).unwrap()).unwrap();
        assert!(load_gold(&path, false).is_ok());
    }

    #[test]
    fn evaluates_a_synthetic_candidate_without_opening_release_gate() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("workspace");
        let index_dir = directory.path().join("index");
        let source = root.join("Qianchuan102_data_process/build.py");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(
            &source,
            "def build_qianchuan_dataset():\n    return 'retrieval fixture'\n",
        )
        .unwrap();
        fs::write(
            root.join("second_relevant.py"),
            "SECOND_ONLY_TOKEN = True\n",
        )
        .unwrap();

        let mut index = WorkspaceIndex::open(&index_dir).unwrap();
        index.index_root(&root, &IndexOptions::default()).unwrap();
        let report = evaluate(
            &index,
            &fixture_gold("candidate_pending_dual_review", "dev"),
        )
        .unwrap();

        assert_eq!(report.recall_at_10, 0.5);
        assert_eq!(report.stale_result_rate, 0.0);
        assert_eq!(report.tool_call_reduction, 0.5);
        assert!(!report.gate.review_approved);
        assert!(!report.gate.release_passed);
    }

    fn fixture_gold(review_status: &str, split: &str) -> RetrievalGoldSet {
        RetrievalGoldSet {
            name: "synthetic-qianchuan".to_owned(),
            asset_family: "qianchuan_distill".to_owned(),
            split: split.to_owned(),
            review_status: review_status.to_owned(),
            reviewers: Vec::new(),
            corpus_roots: vec!["/workspace".to_owned()],
            cases: vec![RetrievalCase {
                id: "code-001".to_owned(),
                category: "code".to_owned(),
                query: "build_qianchuan_dataset".to_owned(),
                relevant_paths: vec![
                    "Qianchuan102_data_process/build.py".to_owned(),
                    "second_relevant.py".to_owned(),
                ],
                baseline_tool_calls: 2,
                evidence: "Synthetic fixture with an exact unique symbol.".to_owned(),
            }],
        }
    }
}
