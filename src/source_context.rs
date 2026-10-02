//! Context preference and copy provenance without per-candidate filesystem reads.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::{SearchHit, SourceAlternative};

pub(crate) fn scope_root(context: &Path) -> PathBuf {
    let directory = if context.is_file() {
        context.parent().unwrap_or(context)
    } else {
        context
    };
    // A linked Git worktree has a .git file, an ordinary checkout a directory.
    directory
        .ancestors()
        .take(64)
        .find(|path| path.join(".git").exists())
        .unwrap_or(directory)
        .to_owned()
}

pub(crate) fn collapse_copies(
    mut candidates: Vec<(SearchHit, Option<String>)>,
) -> Vec<(SearchHit, Option<String>)> {
    candidates.sort_by(|a, b| {
        b.0.score
            .total_cmp(&a.0.score)
            .then_with(|| a.0.path.cmp(&b.0.path))
    });
    let mut groups: HashMap<String, Vec<usize>> = HashMap::new();
    let mut kept: Vec<(SearchHit, Option<String>)> = Vec::new();
    for (mut hit, hash) in candidates {
        let identified = hit.source_context.is_some()
            && (hit.symbol.is_some() || hit.matched_lanes.iter().any(|lane| lane == "exact_path"));
        if !identified {
            kept.push((hit, hash));
            continue;
        }
        let name = Path::new(&hit.path)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let group = groups.entry(name).or_default();
        let identical = group.iter().copied().find(|&index| {
            hit.content_fingerprint
                .as_ref()
                .is_some_and(|hash| kept[index].0.content_fingerprint.as_ref() == Some(hash))
        });
        if let Some(index) = identical {
            add_alternative(&mut kept[index].0, &hit.path, "identical_content");
            if let Some(context) = hit.source_context {
                for alternative in context.alternatives {
                    add_alternative(
                        &mut kept[index].0,
                        &alternative.path,
                        &alternative.relationship,
                    );
                }
            }
            continue;
        }
        for &index in group.iter().take(3) {
            let relationship = if hit.content_fingerprint.is_some()
                && kept[index].0.content_fingerprint.is_some()
            {
                "different_content"
            } else {
                "content_unverified"
            };
            add_alternative(&mut kept[index].0, &hit.path, relationship);
            add_alternative(&mut hit, &kept[index].0.path, relationship);
        }
        group.push(kept.len());
        kept.push((hit, hash));
    }
    kept
}

fn add_alternative(hit: &mut SearchHit, path: &str, relationship: &str) {
    if let Some(context) = &mut hit.source_context
        && context.alternatives.len() < 3
        && !context.alternatives.iter().any(|copy| copy.path == path)
    {
        context.alternatives.push(SourceAlternative {
            path: path.to_owned(),
            relationship: relationship.to_owned(),
        });
    }
}
