use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;
use serde_yaml_ng::Value as YamlValue;

use crate::model::{AgentMemoryLayer, AgentMemoryMetadata, AgentMemorySource};

const PROJECT_PROBE_BYTES: u64 = 512 * 1024;

pub fn discover_agent_memory_sources(
    project_root: &Path,
    include_raw: bool,
) -> Result<Vec<AgentMemorySource>> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is required to discover Agent memories")?;
    discover_agent_memory_sources_in(&home, project_root, include_raw)
}

pub(crate) fn discover_agent_memory_sources_in(
    home: &Path,
    project_root: &Path,
    include_raw: bool,
) -> Result<Vec<AgentMemorySource>> {
    let project_root = fs::canonicalize(project_root)
        .with_context(|| format!("resolve project root {}", project_root.display()))?;
    let mut sources = Vec::new();
    let mut seen = HashSet::new();

    discover_trae(home, &project_root, include_raw, &mut sources, &mut seen)?;
    discover_codex(home, &project_root, include_raw, &mut sources, &mut seen)?;
    discover_zcode(home, &project_root, include_raw, &mut sources, &mut seen)?;
    discover_gemini(home, &project_root, include_raw, &mut sources, &mut seen)?;
    discover_claude(home, &project_root, include_raw, &mut sources, &mut seen)?;

    sources.sort_by(|left, right| {
        left.agent
            .cmp(&right.agent)
            .then_with(|| left.raw_history.cmp(&right.raw_history))
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(sources)
}

pub(crate) fn memory_metadata(
    source: &AgentMemorySource,
    path: &Path,
    source_text: Option<&str>,
    mtime_ns: i64,
) -> AgentMemoryMetadata {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let layer = if source.raw_history {
        AgentMemoryLayer::RawHistory
    } else {
        match name {
            "user_profile.md" => AgentMemoryLayer::UserProfile,
            "project_memory.md" | "MEMORY.md" | "memory_summary.md" => {
                AgentMemoryLayer::ProjectSummary
            }
            "topics.md" => AgentMemoryLayer::TopicSummary,
            _ if name.starts_with("session_memory_")
                || path
                    .components()
                    .any(|part| part.as_os_str() == "rollout_summaries") =>
            {
                AgentMemoryLayer::SessionSummary
            }
            _ => AgentMemoryLayer::MemoryNote,
        }
    };
    let session_id = matches!(
        layer,
        AgentMemoryLayer::SessionSummary | AgentMemoryLayer::RawHistory
    )
    .then(|| session_id(path, source_text))
    .flatten();
    let (frontmatter_name, frontmatter_description) =
        source_text.map(memory_frontmatter).unwrap_or_default();
    AgentMemoryMetadata {
        agent: source.agent.clone(),
        layer,
        name: frontmatter_name,
        description: frontmatter_description,
        workspace_root: source.workspace_root.clone(),
        project_key: source.project_key.clone(),
        session_id,
        observed_at_ms: mtime_ns.saturating_div(1_000_000),
        raw_history: source.raw_history,
    }
}

fn memory_frontmatter(source: &str) -> (Option<String>, Option<String>) {
    let Some(yaml) = frontmatter_yaml(source) else {
        return (None, None);
    };
    let Ok(value) = serde_yaml_ng::from_str::<YamlValue>(yaml) else {
        return (None, None);
    };
    let field = |name: &str, max_chars: usize| {
        value
            .get(name)
            .and_then(YamlValue::as_str)
            .map(|text| {
                text.split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(max_chars)
                    .collect::<String>()
            })
            .filter(|text| !text.is_empty())
    };
    (field("name", 160), field("description", 1_024))
}

fn frontmatter_yaml(source: &str) -> Option<&str> {
    let mut lines = source.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }
    let start = first.len();
    let mut offset = start;
    for line in lines {
        let next = offset + line.len();
        if line.trim() == "---" {
            return Some(&source[start..offset]);
        }
        offset = next;
    }
    None
}

fn discover_trae(
    home: &Path,
    project_root: &Path,
    _include_raw: bool,
    sources: &mut Vec<AgentMemorySource>,
    seen: &mut HashSet<PathBuf>,
) -> Result<()> {
    push_source(
        sources,
        seen,
        home.join(".trae-cn/memory/user_profile.md"),
        "trae",
        None,
        None,
        false,
    )?;
    let projects = home.join(".trae-cn/memory/projects");
    let encoded = encode_project_path(project_root);
    for directory in child_directories(&projects)? {
        let Some(key) = directory
            .file_name()
            .and_then(|value| value.to_str())
            .map(str::to_owned)
        else {
            continue;
        };
        if key == encoded || key.starts_with(&format!("{encoded}--p2-")) {
            push_source(
                sources,
                seen,
                directory,
                "trae",
                Some(project_root),
                Some(&key),
                false,
            )?;
        }
    }
    Ok(())
}

fn discover_codex(
    home: &Path,
    project_root: &Path,
    include_raw: bool,
    sources: &mut Vec<AgentMemorySource>,
    seen: &mut HashSet<PathBuf>,
) -> Result<()> {
    let memories = home.join(".codex/memories");
    for name in ["MEMORY.md", "memory_summary.md"] {
        push_source(
            sources,
            seen,
            memories.join(name),
            "codex",
            Some(project_root),
            Some("codex-curated"),
            false,
        )?;
    }

    let rollout_summaries = memories.join("rollout_summaries");
    for path in child_files(&rollout_summaries)? {
        if path.extension().and_then(|value| value.to_str()) == Some("md")
            && file_mentions_project(&path, project_root)?
        {
            push_source(
                sources,
                seen,
                &path,
                "codex",
                Some(project_root),
                Some("codex-rollout-summary"),
                false,
            )?;
            if include_raw {
                for raw_path in rollout_paths(&path)? {
                    push_source(
                        sources,
                        seen,
                        raw_path,
                        "codex",
                        Some(project_root),
                        Some("codex-session"),
                        true,
                    )?;
                }
            }
        }
    }

    if include_raw {
        push_source(
            sources,
            seen,
            memories.join("raw_memories.md"),
            "codex",
            Some(project_root),
            Some("codex-raw-memory"),
            true,
        )?;
    }
    Ok(())
}

fn discover_zcode(
    home: &Path,
    project_root: &Path,
    include_raw: bool,
    sources: &mut Vec<AgentMemorySource>,
    seen: &mut HashSet<PathBuf>,
) -> Result<()> {
    let zcode = home.join(".zcode/cli");
    let mut mapped_roots = zcode_memory_roots_from_logs(&zcode.join("log"), project_root)?;
    let named_prefix = format!(
        "{}-",
        project_root
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase()
    );
    for project in child_directories(&zcode.join("memories/projects"))? {
        let memory = project.join("memory");
        if project
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with(&named_prefix))
            && directory_has_files(&memory)?
        {
            mapped_roots.push(memory);
        }
    }
    mapped_roots.sort();
    mapped_roots.dedup();
    for root in mapped_roots {
        let key = root
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .map(str::to_owned);
        push_source(
            sources,
            seen,
            root,
            "zcode",
            Some(project_root),
            key.as_deref(),
            false,
        )?;
    }

    if include_raw {
        for base in [zcode.join("rollout"), zcode.join("agents")] {
            for path in recursive_files(&base)? {
                if matches!(
                    path.extension().and_then(|value| value.to_str()),
                    Some("json" | "jsonl" | "md")
                ) && file_mentions_project(&path, project_root)?
                {
                    push_source(
                        sources,
                        seen,
                        path,
                        "zcode",
                        Some(project_root),
                        Some("zcode-raw"),
                        true,
                    )?;
                }
            }
        }
    }
    Ok(())
}

fn discover_gemini(
    home: &Path,
    project_root: &Path,
    include_raw: bool,
    sources: &mut Vec<AgentMemorySource>,
    seen: &mut HashSet<PathBuf>,
) -> Result<()> {
    if !include_raw {
        return Ok(());
    }
    let gemini = home.join(".gemini");
    let projects_path = gemini.join("projects.json");
    let Ok(bytes) = fs::read(&projects_path) else {
        return Ok(());
    };
    let value: Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parse {}", projects_path.display()))?;
    let project_key = value
        .get("projects")
        .and_then(Value::as_object)
        .and_then(|projects| projects.get(project_root.to_string_lossy().as_ref()))
        .and_then(Value::as_str);
    let Some(project_key) = project_key else {
        return Ok(());
    };
    for path in [
        gemini.join("history").join(project_key),
        gemini.join("tmp").join(project_key).join("chats"),
    ] {
        push_source(
            sources,
            seen,
            path,
            "gemini",
            Some(project_root),
            Some(project_key),
            true,
        )?;
    }
    Ok(())
}

fn discover_claude(
    home: &Path,
    project_root: &Path,
    include_raw: bool,
    sources: &mut Vec<AgentMemorySource>,
    seen: &mut HashSet<PathBuf>,
) -> Result<()> {
    let claude = home.join(".claude");
    let encoded = encode_project_path(project_root);
    let project_dir = claude.join("projects").join(&encoded);
    push_source(
        sources,
        seen,
        project_dir.join("memory"),
        "claude",
        Some(project_root),
        Some(&encoded),
        false,
    )?;
    if include_raw {
        for path in recursive_files(&claude.join("sessions"))? {
            if file_mentions_project(&path, project_root)? {
                push_source(
                    sources,
                    seen,
                    path,
                    "claude",
                    Some(project_root),
                    Some(&encoded),
                    true,
                )?;
            }
        }
    }
    Ok(())
}

fn zcode_memory_roots_from_logs(log_dir: &Path, project_root: &Path) -> Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    let project = project_root.to_string_lossy();
    for path in child_files(log_dir)? {
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if !line.contains(project.as_ref()) || !line.contains("\"memoryRoot\"") {
                continue;
            }
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let context = &value["context"];
            if context["workingDirectory"].as_str() != Some(project.as_ref()) {
                continue;
            }
            if let Some(memory_root) = context["memoryRoot"].as_str() {
                roots.push(PathBuf::from(memory_root));
            }
        }
    }
    Ok(roots)
}

fn rollout_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let file = File::open(path)?;
    Ok(BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| line.strip_prefix("rollout_path: ").map(PathBuf::from))
        .collect())
}

fn file_mentions_project(path: &Path, project_root: &Path) -> Result<bool> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(_) => return Ok(false),
    };
    let project = project_root.to_string_lossy();
    let mut reader = BufReader::new(file).take(PROJECT_PROBE_BYTES);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).contains(project.as_ref()))
}

fn push_source(
    sources: &mut Vec<AgentMemorySource>,
    seen: &mut HashSet<PathBuf>,
    path: impl AsRef<Path>,
    agent: &str,
    workspace_root: Option<&Path>,
    project_key: Option<&str>,
    raw_history: bool,
) -> Result<()> {
    let path = path.as_ref();
    if !path.exists() {
        return Ok(());
    }
    let path = fs::canonicalize(path)
        .with_context(|| format!("resolve Agent memory source {}", path.display()))?;
    if !seen.insert(path.clone()) {
        return Ok(());
    }
    sources.push(AgentMemorySource {
        path,
        agent: agent.to_owned(),
        workspace_root: workspace_root.map(Path::to_owned),
        project_key: project_key.map(str::to_owned),
        raw_history,
    });
    Ok(())
}

fn child_directories(path: &Path) -> Result<Vec<PathBuf>> {
    Ok(child_entries(path)?
        .into_iter()
        .filter(|entry| entry.is_dir())
        .collect())
}

fn child_files(path: &Path) -> Result<Vec<PathBuf>> {
    Ok(child_entries(path)?
        .into_iter()
        .filter(|entry| entry.is_file())
        .collect())
}

fn child_entries(path: &Path) -> Result<Vec<PathBuf>> {
    if !path.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(path)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    Ok(entries)
}

fn recursive_files(path: &Path) -> Result<Vec<PathBuf>> {
    if !path.is_dir() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    let mut pending = vec![path.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in child_entries(&directory)? {
            if entry.is_dir() {
                pending.push(entry);
            } else if entry.is_file() {
                files.push(entry);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn directory_has_files(path: &Path) -> Result<bool> {
    Ok(path.is_dir() && fs::read_dir(path)?.any(|entry| entry.is_ok()))
}

fn encode_project_path(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

fn session_id(path: &Path, source: Option<&str>) -> Option<String> {
    let name = path.file_stem()?.to_str()?;
    if let Some(value) = name.strip_prefix("session_memory_") {
        return Some(value.to_owned());
    }
    if let Some(value) = name.strip_prefix("session-") {
        return Some(value.to_owned());
    }
    source.and_then(|source| {
        source.lines().find_map(|line| {
            line.strip_prefix("thread_id: ")
                .or_else(|| {
                    line.strip_prefix("[session_id: ")
                        .and_then(|rest| rest.split_once(" | ").map(|(id, _)| id))
                })
                .map(str::to_owned)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn discovers_only_matching_project_and_keeps_raw_opt_in() {
        let fixture = tempdir().unwrap();
        let home = fixture.path().join("home");
        let project = home.join("Projects/Bona");
        fs::create_dir_all(&project).unwrap();
        let encoded = encode_project_path(&project);
        let trae = home
            .join(".trae-cn/memory/projects")
            .join(format!("{encoded}--p2-test"));
        let other = home.join(".trae-cn/memory/projects/-tmp-other");
        fs::create_dir_all(&trae).unwrap();
        fs::create_dir_all(&other).unwrap();
        fs::write(trae.join("project_memory.md"), "Bona facts").unwrap();
        fs::write(other.join("project_memory.md"), "Other facts").unwrap();
        fs::create_dir_all(home.join(".gemini/tmp/bona/chats")).unwrap();
        fs::write(
            home.join(".gemini/projects.json"),
            format!(r#"{{"projects":{{"{}":"bona"}}}}"#, project.display()),
        )
        .unwrap();

        let curated = discover_agent_memory_sources_in(&home, &project, false).unwrap();
        assert!(curated.iter().any(|source| source.path == trae));
        assert!(curated.iter().all(|source| source.path != other));
        assert!(curated.iter().all(|source| !source.raw_history));

        let raw = discover_agent_memory_sources_in(&home, &project, true).unwrap();
        assert!(raw.iter().any(|source| {
            source.agent == "gemini" && source.raw_history && source.path.ends_with("bona/chats")
        }));
    }
}
