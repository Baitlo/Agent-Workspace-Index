use std::fs;
use std::os::unix::fs::symlink;

use awi::{IndexOptions, QueryInput, QueryRequest, WorkspaceIndex};
use tempfile::tempdir;

#[test]
fn filename_intent_recovers_low_lexical_candidate_and_respects_qualified_path() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::create_dir_all(root.join("other")).unwrap();
    let target = root.join("scripts/run_experiment.py");
    fs::write(&target, "def launch():\n    pass\n").unwrap();
    fs::write(
        root.join("other/run_experiment.py"),
        "def main():\n    pass\n",
    )
    .unwrap();
    for i in 0..110 {
        fs::write(
            root.join(format!("train_eval_{i}.py")),
            "def main():\n    pass\n# train config yaml stage infer eval all mlflow\n",
        )
        .unwrap();
    }
    let mut index = WorkspaceIndex::open(fixture.path().join("index")).unwrap();
    index.index_root(&root, &IndexOptions::default()).unwrap();
    let hits = index
        .search("run_experiment.py stage train config yaml", 5)
        .unwrap();
    assert!(hits[0].path.ends_with("/run_experiment.py"));
    assert!(hits.iter().any(|hit| hit.path == target.to_string_lossy()));
    let hits = index
        .search(
            "`scripts/run_experiment.py` def main --stage train infer eval all mlflow",
            1,
        )
        .unwrap();
    assert_eq!(hits[0].path, target.to_string_lossy());
    assert!(
        hits[0]
            .matched_lanes
            .iter()
            .any(|lane| lane == "exact_path")
    );
}

#[test]
fn child_scope_filters_before_candidate_limit_and_keeps_query_roots_exact() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    let child = root.join("selected");
    fs::create_dir_all(&child).unwrap();
    let target = child.join("target.txt");
    fs::write(&target, "scopeneedle weak").unwrap();
    fs::write(child.join("rows.csv"), "score\n1\n").unwrap();
    for i in 0..220 {
        fs::write(root.join(format!("sibling_{i}.txt")), "scopeneedle").unwrap();
    }
    let mut index = WorkspaceIndex::open(fixture.path().join("index")).unwrap();
    index.index_root(&root, &IndexOptions::default()).unwrap();
    let alias = fixture.path().join("alias");
    symlink(&child, &alias).unwrap();
    for scope in [child.clone(), alias] {
        let hits = index
            .search_filtered("scopeneedle", 1, &[scope], &[], None, None)
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, target.to_string_lossy());
    }
    let outside = fixture.path().join("outside");
    fs::create_dir(&outside).unwrap();
    assert!(
        index
            .search_filtered("scopeneedle", 1, &[outside], &[], None, None)
            .is_err()
    );
    assert!(
        index
            .query(&QueryRequest {
                sql: "SELECT * FROM rows".into(),
                roots: vec![child.clone()],
                inputs: vec![QueryInput {
                    path: child.join("rows.csv"),
                    alias: Some("rows".into())
                }],
                max_rows: 10,
                max_bytes: 4096,
                timeout_ms: 1000,
            })
            .is_err()
    );
}
