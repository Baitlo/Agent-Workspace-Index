use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use rayon::prelude::*;
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TantivyDocument, Value,
};
use tantivy::{Index, Term, doc};

use crate::model::{SearchCandidate, SearchDocument};

const WRITER_HEAP_BYTES: usize = 50_000_000;
const RRF_K: f32 = 60.0;
const PATH_COVERAGE_WEIGHT: f32 = 0.05;
const AGENT_KINDS: &[&str] = &["agent_instructions", "agent_skill"];
const MEMORY_KINDS: &[&str] = &["agent_memory"];

pub(crate) struct SearchIndex {
    index: Index,
    fields: SearchFields,
}

#[derive(Clone, Copy)]
struct SearchFields {
    file_id: Field,
    path_exact: Field,
    path: Field,
    name: Field,
    experiment: Field,
    kind: Field,
    content: Field,
    symbols: Field,
    schema: Field,
    preview: Field,
    generation: Field,
}

struct LaneSpec {
    name: &'static str,
    fields: Vec<Field>,
    weight: f32,
    include_kinds: Option<&'static [&'static str]>,
    exclude_kinds: Option<&'static [&'static str]>,
}

#[derive(Clone)]
struct LaneHit {
    file_id: i64,
    path: String,
    preview: String,
    generation: i64,
}

impl SearchIndex {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        fs::create_dir_all(path)
            .with_context(|| format!("create Tantivy directory {}", path.display()))?;
        let index = if path.join("meta.json").exists() {
            Index::open_in_dir(path)
                .with_context(|| format!("open Tantivy index {}", path.display()))?
        } else {
            Index::create_in_dir(path, build_schema())
                .with_context(|| format!("create Tantivy index {}", path.display()))?
        };
        let schema = index.schema();
        let fields = SearchFields {
            file_id: schema.get_field("file_id")?,
            path_exact: schema.get_field("path_exact")?,
            path: schema.get_field("path")?,
            name: schema.get_field("name")?,
            experiment: schema.get_field("experiment")?,
            kind: schema.get_field("kind")?,
            content: schema.get_field("content")?,
            symbols: schema.get_field("symbols")?,
            schema: schema.get_field("schema")?,
            preview: schema.get_field("preview")?,
            generation: schema.get_field("generation")?,
        };
        Ok(Self { index, fields })
    }

    pub(crate) fn apply_changes(
        &self,
        documents: &[SearchDocument],
        deleted_paths: &[String],
    ) -> Result<()> {
        if documents.is_empty() && deleted_paths.is_empty() {
            return Ok(());
        }

        let mut writer = self
            .index
            .writer(WRITER_HEAP_BYTES)
            .context("create Tantivy writer")?;
        for path in deleted_paths {
            writer.delete_term(Term::from_field_text(self.fields.path_exact, path));
        }
        for document in documents {
            writer.delete_term(Term::from_field_text(
                self.fields.path_exact,
                &document.path,
            ));
            writer.add_document(doc!(
                self.fields.file_id => u64::try_from(document.file_id)
                    .context("negative catalog file id")?,
                self.fields.path_exact => document.path.as_str(),
                self.fields.path => document.path.as_str(),
                self.fields.name => document.name.as_str(),
                self.fields.experiment => document.experiment.as_str(),
                self.fields.kind => document.kind.as_str(),
                self.fields.content => document.content.as_str(),
                self.fields.symbols => document.symbols.as_str(),
                self.fields.schema => document.schema.as_str(),
                self.fields.preview => document.preview.as_str(),
                self.fields.generation => u64::try_from(document.generation)
                    .context("negative generation")?,
            ))?;
        }
        writer.commit().context("commit Tantivy generation")?;
        Ok(())
    }

    pub(crate) fn search(
        &self,
        query: &str,
        lane_limit: usize,
        result_limit: usize,
        include_agent_lane: bool,
    ) -> Result<Vec<SearchCandidate>> {
        let base_exclusions = (!include_agent_lane).then_some(AGENT_KINDS);
        let mut lanes = vec![
            LaneSpec {
                name: "path",
                fields: vec![self.fields.path, self.fields.name, self.fields.experiment],
                weight: 1.25,
                include_kinds: None,
                exclude_kinds: base_exclusions,
            },
            LaneSpec {
                name: "content",
                fields: vec![self.fields.content],
                weight: 1.0,
                include_kinds: None,
                exclude_kinds: base_exclusions,
            },
            LaneSpec {
                name: "symbol",
                fields: vec![self.fields.symbols],
                weight: 1.15,
                include_kinds: None,
                exclude_kinds: base_exclusions,
            },
            LaneSpec {
                name: "schema",
                fields: vec![self.fields.schema],
                weight: 1.1,
                include_kinds: None,
                exclude_kinds: base_exclusions,
            },
        ];
        if include_agent_lane {
            lanes.push(LaneSpec {
                name: "agent",
                fields: vec![self.fields.name, self.fields.symbols, self.fields.content],
                weight: 1.3,
                include_kinds: Some(AGENT_KINDS),
                exclude_kinds: None,
            });
        }
        lanes.push(LaneSpec {
            name: "memory",
            fields: vec![self.fields.name, self.fields.symbols, self.fields.content],
            weight: 1.15,
            include_kinds: Some(MEMORY_KINDS),
            exclude_kinds: None,
        });

        let lane_results = lanes
            .par_iter()
            .map(|lane| {
                self.search_lane(
                    query,
                    &lane.fields,
                    lane.include_kinds,
                    lane.exclude_kinds,
                    lane_limit,
                )
                .map(|hits| (lane.name, lane.weight, hits))
            })
            .collect::<Result<Vec<_>>>()?;

        let mut fused: HashMap<i64, SearchCandidate> = HashMap::new();
        for (lane, weight, hits) in lane_results {
            for (offset, hit) in hits.into_iter().enumerate() {
                let rank = offset as f32 + 1.0;
                let entry = fused.entry(hit.file_id).or_insert_with(|| SearchCandidate {
                    file_id: hit.file_id,
                    path: hit.path,
                    preview: hit.preview,
                    generation: hit.generation,
                    score: 0.0,
                    lanes: Vec::new(),
                });
                entry.score += weight / (RRF_K + rank);
                entry.lanes.push(lane.to_owned());
            }
        }

        let mut candidates = fused.into_values().collect::<Vec<_>>();
        for candidate in &mut candidates {
            candidate.score += PATH_COVERAGE_WEIGHT * path_token_coverage(query, &candidate.path);
            candidate.lanes = candidate
                .lanes
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
        }
        candidates.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| left.path.cmp(&right.path))
        });
        candidates.truncate(result_limit);
        Ok(candidates)
    }

    fn search_lane(
        &self,
        query: &str,
        fields: &[Field],
        include_kinds: Option<&[&str]>,
        exclude_kinds: Option<&[&str]>,
        limit: usize,
    ) -> Result<Vec<LaneHit>> {
        let reader = self.index.reader().context("open Tantivy reader")?;
        let searcher = reader.searcher();
        let parser = QueryParser::for_index(&self.index, fields.to_vec());
        let (text_query, _errors) = parser.parse_query_lenient(query);
        let mut clauses = vec![(tantivy::query::Occur::Must, text_query)];
        if let Some(kinds) = include_kinds {
            let kind_queries = kinds
                .iter()
                .map(|kind| {
                    Box::new(TermQuery::new(
                        Term::from_field_text(self.fields.kind, kind),
                        IndexRecordOption::Basic,
                    )) as Box<dyn Query>
                })
                .collect();
            clauses.push((
                tantivy::query::Occur::Must,
                Box::new(BooleanQuery::union(kind_queries)),
            ));
        }
        if let Some(kinds) = exclude_kinds {
            clauses.extend(kinds.iter().map(|kind| {
                (
                    tantivy::query::Occur::MustNot,
                    Box::new(TermQuery::new(
                        Term::from_field_text(self.fields.kind, kind),
                        IndexRecordOption::Basic,
                    )) as Box<dyn Query>,
                )
            }));
        }
        let query = BooleanQuery::new(clauses);
        let top_docs = searcher.search(&query, &TopDocs::with_limit(limit).order_by_score())?;

        top_docs
            .into_iter()
            .map(|(_score, address)| {
                let document: TantivyDocument = searcher.doc(address)?;
                let file_id = document
                    .get_first(self.fields.file_id)
                    .and_then(|value| value.as_u64())
                    .context("indexed document missing file_id")?;
                let generation = document
                    .get_first(self.fields.generation)
                    .and_then(|value| value.as_u64())
                    .context("indexed document missing generation")?;
                let path = document
                    .get_first(self.fields.path_exact)
                    .and_then(|value| value.as_str())
                    .context("indexed document missing path")?
                    .to_owned();
                let preview = document
                    .get_first(self.fields.preview)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned();
                Ok(LaneHit {
                    file_id: i64::try_from(file_id).context("file_id exceeds i64")?,
                    path,
                    preview,
                    generation: i64::try_from(generation).context("generation exceeds i64")?,
                })
            })
            .collect()
    }
}

fn build_schema() -> Schema {
    let mut builder = Schema::builder();
    builder.add_u64_field("file_id", INDEXED | STORED);
    builder.add_text_field("path_exact", STRING | STORED);
    builder.add_text_field("path", tantivy::schema::TEXT);
    builder.add_text_field("name", tantivy::schema::TEXT);
    builder.add_text_field("experiment", tantivy::schema::TEXT);
    builder.add_text_field("kind", STRING | STORED);
    builder.add_text_field("content", tantivy::schema::TEXT);
    builder.add_text_field("symbols", tantivy::schema::TEXT);
    builder.add_text_field("schema", tantivy::schema::TEXT);
    builder.add_text_field("preview", STORED);
    builder.add_u64_field("generation", STORED);
    builder.build()
}

fn path_token_coverage(query: &str, path: &str) -> f32 {
    let query_tokens = lexical_tokens(query);
    if query_tokens.is_empty() {
        return 0.0;
    }
    let path_tokens = lexical_tokens(path);
    let matched = query_tokens
        .iter()
        .filter(|token| path_tokens.contains(*token))
        .count();
    matched as f32 / query_tokens.len() as f32
}

fn lexical_tokens(value: &str) -> HashSet<String> {
    let mut tokens = HashSet::new();
    let mut current = String::new();
    let mut previous_is_digit = None;
    for character in value.chars() {
        if !character.is_ascii_alphanumeric() {
            if !current.is_empty() {
                tokens.insert(std::mem::take(&mut current));
            }
            previous_is_digit = None;
            continue;
        }
        let is_digit = character.is_ascii_digit();
        if previous_is_digit.is_some_and(|previous| previous != is_digit) && !current.is_empty() {
            tokens.insert(std::mem::take(&mut current));
        }
        current.push(character.to_ascii_lowercase());
        previous_is_digit = Some(is_digit);
    }
    if !current.is_empty() {
        tokens.insert(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_coverage_splits_separators_and_letter_digit_boundaries() {
        let coverage = path_token_coverage(
            "tongyong show_count 500 play_count 500 28 days",
            "/workspace/sql/tongyong_train_v2_show500_play500_28d_optimized.sql",
        );
        assert!(coverage > 0.5, "coverage={coverage}");
    }
}
