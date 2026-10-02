//! Bounded miss diagnostics: catalog lookups and explicit paths only, no walks.
use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::model::FileRecord;
use anyhow::Result;
use serde_json::{Value, json};

pub(crate) fn diagnose(
    anchors: &[String],
    roots: &[PathBuf],
    context: Option<&Path>,
    registered: &[PathBuf],
    lookup: impl Fn(&Path) -> Result<Option<FileRecord>>,
) -> Result<Value> {
    let mut diagnostics = Vec::new();
    let mut bases = Vec::new();
    if let Some(context) = context {
        match fs::canonicalize(context) {
            Ok(path) => {
                let directory = if path.is_file() { path.parent().unwrap_or(&path).to_owned() } else { path };
                bases.push(directory.clone());
                bases.push(crate::source_context::scope_root(&directory));
            }
            Err(_) => diagnostics.push(json!({"reason":"context_unavailable", "path":context,
                "next_action":"Pass an existing task directory as context_path, or omit it; do not repeat the same invalid path."})),
        }
    }
    for root in roots {
        match fs::canonicalize(root) {
            Ok(path) => {
                if !registered
                    .iter()
                    .any(|known| path.starts_with(known) || known.starts_with(&path))
                {
                    diagnostics.push(json!({"reason":"root_outside_indexed_roots","path":root,
                        "next_action":"Use a registered root or ordinary file tools for this scope."}));
                }
                bases.push(path);
            }
            Err(_) => diagnostics.push(json!({"reason":"root_unavailable","path":root,
                "next_action":"Use an existing indexed root; do not retry this missing scope."})),
        }
    }
    if bases.is_empty() {
        bases.extend(
            registered
                .iter()
                .filter(|path| path.is_dir())
                .take(8)
                .cloned(),
        );
    }
    let mut seen = HashSet::new();
    bases.retain(|path| seen.insert(path.clone()));
    let mut probes = 0;
    for anchor in anchors.iter().take(8) {
        let path = Path::new(anchor);
        if path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        {
            diagnostics.push(json!({"anchor":anchor,"reason":"unresolved_path","next_action":"Use a normalized path within the indexed roots."}));
            continue;
        }
        let paths = if path.is_absolute() {
            vec![path.to_owned()]
        } else {
            bases.iter().take(4).map(|base| base.join(path)).collect()
        };
        let mut explained = false;
        for candidate in paths {
            if probes >= 16 {
                break;
            }
            probes += 1;
            let root = registered
                .iter()
                .filter(|root| candidate.starts_with(root))
                .max_by_key(|root| root.components().count());
            let Some(root) = root else {
                if path.is_absolute() {
                    diagnostics.push(json!({"anchor":anchor,"path":candidate,"reason":"outside_indexed_roots","next_action":"Use ordinary file tools for this path or explicitly register its directory."}));
                    explained = true;
                }
                continue;
            };
            // Never inspect content, credentials, or a path reached through a symlink.
            if crate::extract::is_sensitive(&candidate)
                || crate::extract::is_default_excluded(
                    candidate.strip_prefix(root).unwrap_or(&candidate),
                )
            {
                diagnostics.push(json!({"anchor":anchor,"path":candidate,"reason":"excluded_by_policy","next_action":"Respect the indexing exclusions; choose an allowed source path."}));
                explained = true;
                continue;
            }
            if candidate.strip_prefix(root).ok().is_some_and(|relative| {
                let mut prefix = root.clone();
                relative.components().any(|part| {
                    prefix.push(part);
                    fs::symlink_metadata(&prefix).is_ok_and(|meta| meta.file_type().is_symlink())
                })
            }) {
                diagnostics.push(json!({"anchor":anchor,"path":candidate,"reason":"symlink_not_followed","next_action":"Use an explicitly registered canonical path."}));
                explained = true;
                continue;
            }
            if let Some(file) = lookup(&candidate)? {
                diagnostics.push(json!({"anchor":anchor,"path":candidate,"reason":"indexed_not_returned","generation":file.generation,
                    "next_action":"Check roots/kinds/path_prefix and search with the qualified path, or inspect this catalog-confirmed file."}));
                explained = true;
            } else if let Some(rule) = ignored_rule(root, &candidate) {
                diagnostics.push(json!({"anchor":anchor,"path":candidate,"reason":"ignored","rule":rule,
                    "next_action":"This explicit candidate matches an ignore rule. Use file tools if appropriate; change indexing scope only explicitly."}));
                explained = true;
            } else if fs::metadata(&candidate).is_ok_and(|meta| meta.is_file()) {
                diagnostics.push(json!({"anchor":anchor,"path":candidate,"reason":"present_not_indexed",
                    "next_action":"Use file tools now; check producer coverage or extraction failures before requesting a reconcile."}));
                explained = true;
            }
        }
        if !explained {
            diagnostics.push(json!({"anchor":anchor,"reason":"not_returned","next_action":"No explanation from bounded explicit-path checks. Locate the path with file tools; absence from the workspace is not established."}));
        }
    }
    if anchors.is_empty() && diagnostics.is_empty() {
        diagnostics.push(json!({"reason":"no_matching_evidence","next_action":"Check query terms and filters. Refine once or use file tools; coverage is not proven complete."}));
    }
    Ok(
        json!({"scope":"bounded_explicit_paths","probed_paths":probes,"workspace_completeness":"unknown","items":diagnostics}),
    )
}

fn ignored_rule(root: &Path, path: &Path) -> Option<Value> {
    // Load only bounded ancestors of the explicit candidate. No directory listing.
    let parents: Vec<_> = path.parent()?.ancestors().take(64).collect();
    let git_boundary = parents
        .iter()
        .position(|parent| parent.join(".git").exists());
    let mut layers = Vec::new();
    for name in [".awiignore", ".ignore", ".gitignore", ".git/info/exclude"] {
        let mut layer = Vec::new();
        for (depth, parent) in parents.iter().enumerate() {
            if name.starts_with(".git") && git_boundary.is_none_or(|boundary| depth > boundary) {
                continue;
            }
            let file = parent.join(name);
            if !fs::symlink_metadata(&file)
                .is_ok_and(|meta| meta.is_file() && meta.len() <= 64 * 1024)
            {
                continue;
            }
            let mut builder = ignore::gitignore::GitignoreBuilder::new(parent);
            if builder.add(&file).is_none()
                && let Ok(matcher) = builder.build()
            {
                layer.push((*parent, matcher));
            }
        }
        layers.push(layer);
    }
    let mut candidates: Vec<_> = path
        .ancestors()
        .take_while(|candidate| *candidate != root && candidate.starts_with(root))
        .collect();
    candidates.reverse();
    // A pruned directory cannot be re-included by a deeper ignore file.
    for candidate in candidates {
        let is_dir = candidate != path;
        'priority: for layer in &layers {
            for (parent, matcher) in layer {
                if !candidate.starts_with(parent) || candidate == *parent {
                    continue;
                }
                match matcher.matched(candidate, is_dir) {
                    ignore::Match::Ignore(glob) => {
                        return Some(
                            json!({"file":glob.from(),"pattern":glob.original(),"matched_path":candidate}),
                        );
                    }
                    ignore::Match::Whitelist(_) => break 'priority,
                    ignore::Match::None => {}
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignored_parents_and_custom_precedence_match_indexing() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join(".git")).unwrap();
        fs::create_dir_all(root.join("excluded/deep")).unwrap();
        fs::write(root.join(".gitignore"), "excluded/\nfile.py\n").unwrap();
        fs::write(root.join("excluded/.gitignore"), "!deep/file.py\n").unwrap();
        assert_eq!(
            ignored_rule(root, &root.join("excluded/deep/file.py")).unwrap()["pattern"],
            "excluded/"
        );
        fs::write(root.join(".awiignore"), "!file.py\n").unwrap();
        assert!(ignored_rule(root, &root.join("file.py")).is_none());
        fs::write(root.join(".ignore"), "other.py\n").unwrap();
        assert_eq!(
            ignored_rule(root, &root.join("other.py")).unwrap()["pattern"],
            "other.py"
        );
        fs::create_dir_all(root.join("nested/.git")).unwrap();
        assert!(ignored_rule(root, &root.join("nested/file.py")).is_none());
    }
}
