use std::fs::File;
use std::io::{Read, Take};
use std::path::{Component, Path};
use std::sync::OnceLock;
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use pulldown_cmark::{Event, Parser as MarkdownParser, Tag, TagEnd};
use regex::RegexSet;
use serde_yaml_ng::Value as YamlValue;
use tree_sitter::{Language, Node, Parser as TreeSitterParser};

use crate::model::{AgentDocumentMetadata, AgentDocumentRole, FileKind, SymbolRecord};

const INDEX_PREVIEW_CHARS: usize = 2_000;
const MAX_AGENT_HEADINGS: usize = 64;
const MAX_AGENT_REFERENCES: usize = 64;

pub(crate) struct TextExtraction {
    pub content: Option<String>,
    pub content_hash: Option<String>,
    pub preview: String,
    pub status: String,
}

pub(crate) fn classify(path: &Path) -> FileKind {
    match path.file_name().and_then(|value| value.to_str()) {
        Some("AGENTS.md") => return FileKind::AgentInstructions,
        Some("SKILL.md") => return FileKind::AgentSkill,
        _ => {}
    }
    let extension = extension(path);
    match extension.as_deref() {
        Some(
            "rs" | "go" | "py" | "pyi" | "js" | "jsx" | "ts" | "tsx" | "java" | "kt" | "kts" | "c"
            | "h" | "cc" | "cpp" | "hpp" | "cs" | "rb" | "php" | "swift" | "scala" | "sh" | "bash",
        ) => FileKind::Source,
        Some(
            "sql" | "md" | "txt" | "log" | "yaml" | "yml" | "toml" | "ini" | "conf" | "cfg" | "xml"
            | "html" | "css",
        ) => FileKind::Text,
        Some("json") => FileKind::SemiStructured,
        Some("csv" | "tsv" | "jsonl" | "ndjson" | "parquet") => FileKind::Tabular,
        Some(
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "mp4" | "mov" | "avi" | "pdf" | "zip" | "tar"
            | "gz" | "zst" | "bin" | "pt" | "pth" | "safetensors" | "onnx",
        ) => FileKind::Binary,
        Some(_) => FileKind::Unknown,
        None => FileKind::Unknown,
    }
}

pub(crate) fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
}

pub(crate) fn should_extract_text(kind: FileKind, size_bytes: u64, max_bytes: u64) -> bool {
    matches!(
        kind,
        FileKind::Source
            | FileKind::Text
            | FileKind::SemiStructured
            | FileKind::Tabular
            | FileKind::AgentInstructions
            | FileKind::AgentSkill
    ) && size_bytes <= max_bytes
        && kind != FileKind::Binary
}

pub(crate) fn extract_text(path: &Path, max_bytes: u64) -> Result<TextExtraction> {
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut bytes = Vec::new();
    let mut reader: Take<File> = file.take(max_bytes.saturating_add(1));
    reader
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {}", path.display()))?;

    if bytes.len() as u64 > max_bytes {
        return Ok(TextExtraction {
            content: None,
            content_hash: None,
            preview: String::new(),
            status: "metadata_only_oversized".to_owned(),
        });
    }
    if bytes.contains(&0) {
        return Ok(TextExtraction {
            content: None,
            content_hash: None,
            preview: String::new(),
            status: "metadata_only_binary".to_owned(),
        });
    }

    let content_hash = blake3::hash(&bytes).to_hex().to_string();
    let content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(_) => {
            return Ok(TextExtraction {
                content: None,
                content_hash: Some(content_hash),
                preview: String::new(),
                status: "metadata_only_invalid_utf8".to_owned(),
            });
        }
    };
    let preview = content.chars().take(INDEX_PREVIEW_CHARS).collect();
    Ok(TextExtraction {
        content: Some(content),
        content_hash: Some(content_hash),
        preview,
        status: "indexed".to_owned(),
    })
}

pub(crate) fn extract_symbols(path: &Path, source: &str) -> Result<(Vec<SymbolRecord>, String)> {
    let Some((language, language_name)) = parser_language(path) else {
        return Ok((Vec::new(), "unsupported_language".to_owned()));
    };

    let mut parser = TreeSitterParser::new();
    parser
        .set_language(&language)
        .with_context(|| format!("load {language_name} tree-sitter grammar"))?;
    let Some(tree) = parser.parse(source, None) else {
        return Ok((Vec::new(), "parse_failed".to_owned()));
    };

    let mut symbols = Vec::new();
    collect_symbols(
        tree.root_node(),
        source.as_bytes(),
        language_name,
        &mut symbols,
    );
    let status = if tree.root_node().has_error() {
        "parsed_with_errors"
    } else {
        "parsed"
    };
    Ok((symbols, status.to_owned()))
}

pub(crate) fn extract_agent_document(
    root: &Path,
    path: &Path,
    source: &str,
) -> Result<Option<AgentDocumentMetadata>> {
    let role = match classify(path) {
        FileKind::AgentInstructions => AgentDocumentRole::Instructions,
        FileKind::AgentSkill => AgentDocumentRole::Skill,
        _ => return Ok(None),
    };
    let base = if root.is_dir() {
        root
    } else {
        root.parent().unwrap_or(root)
    };
    let document_dir = path.parent().unwrap_or(base);
    let scope_root = match role {
        AgentDocumentRole::Instructions => document_dir.to_owned(),
        AgentDocumentRole::Skill => base.to_owned(),
    };
    let precedence_depth = scope_root.components().count();
    let (name, description) = skill_frontmatter(source)?;
    let (headings, references) = markdown_structure(base, document_dir, source);

    Ok(Some(AgentDocumentMetadata {
        role,
        name,
        description,
        scope_root,
        precedence_depth,
        headings,
        references,
    }))
}

pub(crate) fn mtime_ns(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| {
            i64::try_from(duration.as_nanos())
                .unwrap_or(i64::MAX)
                .max(0)
        })
        .unwrap_or_default()
}

pub(crate) fn is_sensitive(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name == ".env"
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.contains("credential")
        || name.contains("access_token")
        || name.contains("secret_key")
}

pub(crate) fn contains_sensitive_agent_content(source: &str) -> bool {
    static PATTERNS: OnceLock<RegexSet> = OnceLock::new();
    PATTERNS
        .get_or_init(|| {
            RegexSet::new([
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----",
                r"\bAKIA[0-9A-Z]{16}\b",
                r"\bgithub_pat_[A-Za-z0-9_]{30,}\b",
                r"\bgh[pousr]_[A-Za-z0-9]{30,}\b",
                r"\bglpat-[A-Za-z0-9_-]{20,}\b",
                r"\bxox[baprs]-[A-Za-z0-9-]{20,}\b",
                r"\bsk-(?:ant-)?[A-Za-z0-9_-]{24,}\b",
            ])
            .expect("valid Agent secret patterns")
        })
        .is_match(source)
}

pub(crate) fn is_default_excluded(path: &Path) -> bool {
    path.components().any(|component| {
        let Component::Normal(value) = component else {
            return false;
        };
        matches!(
            value.to_str(),
            Some(
                ".git"
                    | ".hg"
                    | ".svn"
                    | ".awi-index"
                    | "node_modules"
                    | "target"
                    | "__pycache__"
                    | ".pytest_cache"
                    | ".mypy_cache"
                    | ".ruff_cache"
                    | ".ipynb_checkpoints"
                    | ".venv"
                    | ".idea"
                    | ".vscode"
                    | ".cache"
            )
        )
    })
}

fn parser_language(path: &Path) -> Option<(Language, &'static str)> {
    match extension(path).as_deref() {
        Some("rs") => Some((tree_sitter_rust::LANGUAGE.into(), "rust")),
        Some("py" | "pyi") => Some((tree_sitter_python::LANGUAGE.into(), "python")),
        Some("go") => Some((tree_sitter_go::LANGUAGE.into(), "go")),
        _ => None,
    }
}

fn skill_frontmatter(source: &str) -> Result<(Option<String>, Option<String>)> {
    let Some((yaml, _)) = split_frontmatter(source) else {
        return Ok((None, None));
    };
    let value: YamlValue =
        serde_yaml_ng::from_str(yaml).context("parse Agent Markdown frontmatter")?;
    let name = value
        .get("name")
        .and_then(YamlValue::as_str)
        .map(|value| compact(value, 160))
        .filter(|value| !value.is_empty());
    let description = value
        .get("description")
        .and_then(YamlValue::as_str)
        .map(|value| compact(value, 1_024))
        .filter(|value| !value.is_empty());
    Ok((name, description))
}

fn markdown_structure(
    allowed_root: &Path,
    document_dir: &Path,
    source: &str,
) -> (Vec<String>, Vec<std::path::PathBuf>) {
    let mut headings = Vec::new();
    let mut current_heading = None::<String>;
    let mut references = Vec::new();
    let source = split_frontmatter(source).map_or(source, |(_, body)| body);
    for event in MarkdownParser::new(source) {
        match event {
            Event::Start(Tag::Heading { .. }) => current_heading = Some(String::new()),
            Event::End(TagEnd::Heading(_)) => {
                if let Some(heading) = current_heading.take() {
                    let heading = compact(&heading, 256);
                    if !heading.is_empty() && headings.len() < MAX_AGENT_HEADINGS {
                        headings.push(heading);
                    }
                }
            }
            Event::Text(text) | Event::Code(text) => {
                if let Some(heading) = &mut current_heading {
                    if !heading.is_empty() {
                        heading.push(' ');
                    }
                    heading.push_str(&text);
                }
            }
            Event::Start(Tag::Link { dest_url, .. }) if references.len() < MAX_AGENT_REFERENCES => {
                if let Some(path) =
                    resolve_local_reference(allowed_root, document_dir, dest_url.as_ref())
                    && !references.contains(&path)
                {
                    references.push(path);
                }
            }
            _ => {}
        }
    }
    (headings, references)
}

fn split_frontmatter(source: &str) -> Option<(&str, &str)> {
    let mut lines = source.split_inclusive('\n');
    let first = lines.next()?;
    if first.trim() != "---" {
        return None;
    }
    let yaml_start = first.len();
    let mut offset = yaml_start;
    for line in lines {
        let next = offset + line.len();
        if line.trim() == "---" {
            return Some((&source[yaml_start..offset], &source[next..]));
        }
        offset = next;
    }
    None
}

fn resolve_local_reference(
    allowed_root: &Path,
    document_dir: &Path,
    destination: &str,
) -> Option<std::path::PathBuf> {
    let destination = destination.split('#').next()?.trim();
    if destination.is_empty()
        || destination.starts_with('/')
        || destination.contains("://")
        || destination.starts_with("mailto:")
    {
        return None;
    }
    let path = document_dir.join(destination);
    let path = std::fs::canonicalize(path).ok()?;
    (path.is_file() && path.starts_with(allowed_root)).then_some(path)
}

fn collect_symbols(node: Node<'_>, source: &[u8], language: &str, symbols: &mut Vec<SymbolRecord>) {
    if let Some(kind) = symbol_kind(node.kind()) {
        let name_node = node
            .child_by_field_name("name")
            .or_else(|| node.child_by_field_name("function"))
            .or_else(|| node.child_by_field_name("path"))
            .or_else(|| node.named_child(0));
        if let Some(name_node) = name_node
            && let Ok(name) = name_node.utf8_text(source)
        {
            let name = compact(name, 160);
            if !name.is_empty() {
                let signature = node
                    .utf8_text(source)
                    .ok()
                    .map(|text| compact(text.lines().next().unwrap_or_default(), 240))
                    .filter(|text| !text.is_empty());
                symbols.push(SymbolRecord {
                    name,
                    kind: kind.to_owned(),
                    language: language.to_owned(),
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    signature,
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_symbols(child, source, language, symbols);
    }
}

fn symbol_kind(node_kind: &str) -> Option<&'static str> {
    match node_kind {
        "function_item" | "function_definition" | "function_declaration" => Some("function"),
        "method_declaration" => Some("method"),
        "class_definition" => Some("class"),
        "struct_item" => Some("struct"),
        "enum_item" => Some("enum"),
        "trait_item" => Some("trait"),
        "type_spec" | "type_declaration" => Some("type"),
        "use_declaration" | "import_statement" | "import_from_statement" | "import_declaration" => {
            Some("import")
        }
        "call_expression" => Some("call"),
        _ => None,
    }
}

fn compact(value: &str, max_chars: usize) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn classifies_supported_artifacts() {
        assert_eq!(classify(Path::new("src/main.rs")), FileKind::Source);
        assert_eq!(classify(Path::new("query.sql")), FileKind::Text);
        assert_eq!(classify(Path::new("rows.tsv")), FileKind::Tabular);
        assert_eq!(classify(Path::new("rows.jsonl")), FileKind::Tabular);
        assert_eq!(classify(Path::new("part.parquet")), FileKind::Tabular);
        assert_eq!(
            classify(Path::new("nested/AGENTS.md")),
            FileKind::AgentInstructions
        );
        assert_eq!(
            classify(Path::new(".agents/skills/awi/SKILL.md")),
            FileKind::AgentSkill
        );
        assert_eq!(classify(Path::new("model.safetensors")), FileKind::Binary);
    }

    #[test]
    fn extracts_skill_frontmatter_headings_and_local_references() {
        let fixture = tempdir().unwrap();
        let root = fixture.path();
        let skill_dir = root.join("skills/awi");
        fs::create_dir_all(skill_dir.join("references")).unwrap();
        fs::write(
            skill_dir.join("references/architecture.md"),
            "# Architecture\n",
        )
        .unwrap();
        let path = skill_dir.join("SKILL.md");
        let source = r#"---
name: awi
description: >-
  Search code, data, and Agent knowledge.
---

# AWI

Read [architecture](references/architecture.md) before changes.

## Workflow
"#;
        fs::write(&path, source).unwrap();

        let metadata = extract_agent_document(root, &path, source)
            .unwrap()
            .unwrap();
        assert_eq!(metadata.role, AgentDocumentRole::Skill);
        assert_eq!(metadata.name.as_deref(), Some("awi"));
        assert_eq!(
            metadata.description.as_deref(),
            Some("Search code, data, and Agent knowledge.")
        );
        assert_eq!(metadata.scope_root, root);
        assert_eq!(metadata.precedence_depth, root.components().count());
        assert_eq!(metadata.headings, vec!["AWI", "Workflow"]);
        assert_eq!(
            metadata.references,
            vec![fs::canonicalize(skill_dir.join("references/architecture.md")).unwrap()]
        );
    }

    #[test]
    fn extracts_instruction_scope_from_its_parent_directory() {
        let fixture = tempdir().unwrap();
        let root = fixture.path();
        let path = root.join("services/api/AGENTS.md");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "# API Rules\n").unwrap();

        let metadata = extract_agent_document(root, &path, "# API Rules\n")
            .unwrap()
            .unwrap();
        assert_eq!(metadata.role, AgentDocumentRole::Instructions);
        assert_eq!(metadata.scope_root, root.join("services/api"));
        assert_eq!(
            metadata.precedence_depth,
            root.join("services/api").components().count()
        );
        assert_eq!(metadata.headings, vec!["API Rules"]);
    }

    #[test]
    fn rejects_malformed_skill_frontmatter() {
        let fixture = tempdir().unwrap();
        let path = fixture.path().join("SKILL.md");
        let source = "---\nname: [unterminated\n---\n# Broken\n";
        fs::write(&path, source).unwrap();
        assert!(extract_agent_document(fixture.path(), &path, source).is_err());
    }

    #[test]
    fn detects_high_confidence_secrets_without_rejecting_placeholders() {
        assert!(contains_sensitive_agent_content(
            "token = ghp_abcdefghijklmnopqrstuvwxyz1234567890"
        ));
        assert!(contains_sensitive_agent_content(
            "-----BEGIN OPENSSH PRIVATE KEY-----"
        ));
        assert!(!contains_sensitive_agent_content(
            "Set API_KEY to YOUR_API_KEY before running."
        ));
    }

    #[test]
    fn extracts_rust_symbols() {
        let source = "use std::path::Path;\nfn build_index() { helper(); }\n";
        let (symbols, status) = extract_symbols(Path::new("lib.rs"), source).unwrap();
        assert_eq!(status, "parsed");
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "build_index" && symbol.kind == "function")
        );
        assert!(
            symbols
                .iter()
                .any(|symbol| symbol.name == "helper" && symbol.kind == "call")
        );
    }

    #[test]
    fn excludes_vcs_cache_and_build_directories() {
        for path in [
            "repo/.git/objects/ab/cd",
            "repo/.hg/store",
            "repo/.svn/entries",
            "repo/node_modules/pkg/index.js",
            "repo/target/debug/build.rs",
            "repo/__pycache__/mod.pyc",
            "repo/.pytest_cache/v/cache",
            "repo/.mypy_cache/3.11/x.json",
            "repo/.ruff_cache/content",
            "repo/.ipynb_checkpoints/nb-checkpoint.ipynb",
            "repo/.venv/bin/python",
            "repo/.idea/workspace.xml",
            "repo/.vscode/settings.json",
            "repo/.cache/blob",
            "repo/.awi-index/catalog.sqlite3",
        ] {
            assert!(
                is_default_excluded(Path::new(path)),
                "expected {path} to be excluded"
            );
        }
        // Ordinary source and data are not excluded.
        assert!(!is_default_excluded(Path::new("repo/src/main.rs")));
        assert!(!is_default_excluded(Path::new("repo/data/metrics.csv")));
        // A substring match must not trigger: "targets" is not "target".
        assert!(!is_default_excluded(Path::new("repo/targets/list.txt")));
    }
}
