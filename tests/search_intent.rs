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

#[test]
fn context_prefers_worktree_and_collapses_only_identical_named_copies() {
    let fixture = tempdir().unwrap();
    let root = fixture.path().join("workspace");
    for name in ["a_old", "b_copy", "z_current"] {
        fs::create_dir_all(root.join(name).join("src")).unwrap();
        // Both .git files and directories are valid checkout boundaries.
        fs::write(root.join(name).join(".git"), "gitdir: /unused\n").unwrap();
    }
    let current = root.join("z_current/src/engine.py");
    let old = root.join("a_old/src/engine.py");
    let copy = root.join("b_copy/src/engine.py");
    fs::write(&current, "def decode_formula():\n    return 'current'\n").unwrap();
    fs::write(&copy, "def decode_formula():\n    return 'current'\n").unwrap();
    fs::write(&old, "def decode_formula():\n    return 'old'\n").unwrap();
    let mut index = WorkspaceIndex::open(fixture.path().join("index")).unwrap();
    index.index_root(&root, &IndexOptions::default()).unwrap();
    let context = root.join("z_current/src");
    for query in ["engine.py", "decode_formula"] {
        let hits = index
            .search_filtered(query, 5, &[], &["source".into()], None, Some(&context))
            .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].path, current.to_string_lossy());
        let provenance = hits[0].source_context.as_ref().unwrap();
        assert!(provenance.in_scope);
        assert_eq!(
            provenance.scope_root,
            root.join("z_current").to_string_lossy()
        );
        assert!(
            provenance
                .alternatives
                .iter()
                .any(|entry| entry.path == copy.to_string_lossy()
                    && entry.relationship == "identical_content")
        );
        assert!(
            provenance
                .alternatives
                .iter()
                .any(|entry| entry.path == old.to_string_lossy()
                    && entry.relationship == "different_content")
        );
        assert!(hits.iter().any(|hit| hit.path == old.to_string_lossy()));
    }
    let explicit = index
        .search_filtered("a_old/src/engine.py", 1, &[], &[], None, Some(&context))
        .unwrap();
    assert_eq!(explicit[0].path, old.to_string_lossy());
    let unscoped = index
        .search_filtered("engine.py", 5, &[], &["source".into()], None, None)
        .unwrap();
    assert_eq!(unscoped.len(), 3);
    assert!(unscoped.iter().all(|hit| hit.source_context.is_none()));
}
