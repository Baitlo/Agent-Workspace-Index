use std::fs::File;
use std::io::{Read, Take};
use std::path::{Component, Path};
use std::time::UNIX_EPOCH;

use anyhow::{Context, Result};
use tree_sitter::{Language, Node, Parser};

use crate::model::{FileKind, SymbolRecord};

const INDEX_PREVIEW_CHARS: usize = 2_000;

pub(crate) struct TextExtraction {
    pub content: Option<String>,
    pub content_hash: Option<String>,
    pub preview: String,
    pub status: String,
}

pub(crate) fn classify(path: &Path) -> FileKind {
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
        FileKind::Source | FileKind::Text | FileKind::SemiStructured | FileKind::Tabular
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

    let mut parser = Parser::new();
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
    use super::*;

    #[test]
    fn classifies_supported_artifacts() {
        assert_eq!(classify(Path::new("src/main.rs")), FileKind::Source);
        assert_eq!(classify(Path::new("query.sql")), FileKind::Text);
        assert_eq!(classify(Path::new("rows.tsv")), FileKind::Tabular);
        assert_eq!(classify(Path::new("rows.jsonl")), FileKind::Tabular);
        assert_eq!(classify(Path::new("part.parquet")), FileKind::Tabular);
        assert_eq!(classify(Path::new("model.safetensors")), FileKind::Binary);
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
