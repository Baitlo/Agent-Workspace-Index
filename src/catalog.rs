use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use crate::model::{
    AgentDocumentMetadata, AgentDocumentRole, AgentMemoryLayer, AgentMemoryMetadata,
    AgentMemorySource, CatalogFileInput, DatasetProfile, ExistingFile, FileKind, FileRecord,
    IndexStatus, SymbolRecord,
};

pub(crate) struct Catalog {
    connection: Connection,
}

impl Catalog {
    pub(crate) fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create catalog directory {}", parent.display()))?;
        }

        let connection =
            Connection::open(path).with_context(|| format!("open catalog {}", path.display()))?;
        connection.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS roots (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                last_completed_generation INTEGER
            );

            CREATE TABLE IF NOT EXISTS generations (
                id INTEGER PRIMARY KEY,
                root_id INTEGER NOT NULL REFERENCES roots(id),
                started_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER,
                status TEXT NOT NULL,
                error TEXT
            );

            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY,
                root_id INTEGER NOT NULL REFERENCES roots(id),
                absolute_path TEXT NOT NULL UNIQUE,
                relative_path TEXT NOT NULL,
                name TEXT NOT NULL,
                extension TEXT,
                kind TEXT NOT NULL,
                experiment TEXT,
                size_bytes INTEGER NOT NULL,
                mtime_ns INTEGER NOT NULL,
                content_hash TEXT,
                index_generation INTEGER NOT NULL,
                generation_completed INTEGER NOT NULL DEFAULT 1,
                last_seen_generation INTEGER NOT NULL,
                content_indexed INTEGER NOT NULL,
                extraction_status TEXT NOT NULL,
                deleted INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS files_root_seen
                ON files(root_id, last_seen_generation);
            CREATE INDEX IF NOT EXISTS files_generation
                ON files(index_generation, deleted);

            CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                kind TEXT NOT NULL,
                language TEXT NOT NULL,
                line_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                signature TEXT
            );

            CREATE INDEX IF NOT EXISTS symbols_file ON symbols(file_id);
            CREATE INDEX IF NOT EXISTS symbols_name ON symbols(name);

            CREATE TABLE IF NOT EXISTS dataset_profiles (
                file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                format TEXT NOT NULL,
                status TEXT NOT NULL,
                profile_json TEXT NOT NULL,
                profiler TEXT NOT NULL,
                error TEXT
            );

            CREATE TABLE IF NOT EXISTS agent_documents (
                file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                name TEXT,
                description TEXT,
                scope_root TEXT NOT NULL,
                precedence_depth INTEGER NOT NULL,
                headings_json TEXT NOT NULL,
                references_json TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS agent_documents_role
                ON agent_documents(role);

            CREATE TABLE IF NOT EXISTS agent_memory_sources (
                path TEXT PRIMARY KEY,
                agent TEXT NOT NULL,
                workspace_root TEXT,
                project_key TEXT,
                raw_history INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS agent_memory_sources_workspace
                ON agent_memory_sources(workspace_root);

            CREATE TABLE IF NOT EXISTS agent_memories (
                file_id INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
                agent TEXT NOT NULL,
                layer TEXT NOT NULL,
                workspace_root TEXT,
                project_key TEXT,
                session_id TEXT,
                observed_at_ms INTEGER NOT NULL,
                raw_history INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS agent_memories_workspace
                ON agent_memories(workspace_root, layer);

            CREATE TABLE IF NOT EXISTS lineage_edges (
                id INTEGER PRIMARY KEY,
                source_file_id INTEGER REFERENCES files(id),
                target TEXT NOT NULL,
                relation TEXT NOT NULL,
                generation INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS index_failures (
                id INTEGER PRIMARY KEY,
                generation INTEGER NOT NULL,
                path TEXT NOT NULL,
                stage TEXT NOT NULL,
                error TEXT NOT NULL
            );
            ",
        )?;
        ensure_column(
            &connection,
            "files",
            "generation_completed",
            "INTEGER NOT NULL DEFAULT 1",
        )?;

        Ok(Self { connection })
    }

    pub(crate) fn start_generation(&mut self, root: &Path) -> Result<(i64, i64)> {
        self.connection.execute(
            "UPDATE generations
             SET status = 'failed', completed_at_ms = ?1,
                 error = COALESCE(error, 'interrupted before completion')
             WHERE status = 'running'",
            params![now_ms()],
        )?;
        let root = root.to_string_lossy();
        self.connection.execute(
            "INSERT INTO roots(path) VALUES (?1)
             ON CONFLICT(path) DO NOTHING",
            params![root.as_ref()],
        )?;
        let root_id = self.connection.query_row(
            "SELECT id FROM roots WHERE path = ?1",
            params![root.as_ref()],
            |row| row.get(0),
        )?;
        self.connection.execute(
            "INSERT INTO generations(root_id, started_at_ms, status)
             VALUES (?1, ?2, 'running')",
            params![root_id, now_ms()],
        )?;
        Ok((root_id, self.connection.last_insert_rowid()))
    }

    pub(crate) fn roots(&self) -> Result<Vec<(i64, PathBuf)>> {
        let mut statement = self
            .connection
            .prepare("SELECT id, path FROM roots ORDER BY length(path) DESC, path")?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    PathBuf::from(row.get::<_, String>(1)?),
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("list indexed roots")
    }

    pub(crate) fn upsert_agent_memory_source(&self, source: &AgentMemorySource) -> Result<()> {
        self.connection.execute(
            "INSERT INTO agent_memory_sources(
                path, agent, workspace_root, project_key, raw_history
             ) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET
                agent = excluded.agent,
                workspace_root = excluded.workspace_root,
                project_key = excluded.project_key,
                raw_history = excluded.raw_history",
            params![
                source.path.to_string_lossy().as_ref(),
                source.agent,
                source
                    .workspace_root
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                source.project_key,
                source.raw_history,
            ],
        )?;
        Ok(())
    }

    pub(crate) fn agent_memory_source_for_path(
        &self,
        path: &Path,
    ) -> Result<Option<AgentMemorySource>> {
        let path = path.to_string_lossy();
        let raw = self
            .connection
            .query_row(
                "SELECT path, agent, workspace_root, project_key, raw_history
                 FROM agent_memory_sources
                 WHERE ?1 = path OR substr(?1, 1, length(path) + 1) = path || '/'
                 ORDER BY length(path) DESC
                 LIMIT 1",
                params![path.as_ref()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, bool>(4)?,
                    ))
                },
            )
            .optional()?;
        Ok(raw.map(
            |(path, agent, workspace_root, project_key, raw_history)| AgentMemorySource {
                path: PathBuf::from(path),
                agent,
                workspace_root: workspace_root.map(PathBuf::from),
                project_key,
                raw_history,
            },
        ))
    }

    pub(crate) fn complete_generation(&mut self, root_id: i64, generation: i64) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE generations
             SET status = 'completed', completed_at_ms = ?1
             WHERE id = ?2 AND status = 'running'",
            params![now_ms(), generation],
        )?;
        transaction.execute(
            "UPDATE roots SET last_completed_generation = ?1 WHERE id = ?2",
            params![generation, root_id],
        )?;
        transaction.execute(
            "UPDATE files SET generation_completed = 1 WHERE index_generation = ?1",
            params![generation],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn fail_generation(&self, generation: i64, error: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE generations
             SET status = 'failed', completed_at_ms = ?1, error = ?2
             WHERE id = ?3",
            params![now_ms(), error, generation],
        )?;
        Ok(())
    }

    pub(crate) fn existing_file(&self, absolute_path: &str) -> Result<Option<ExistingFile>> {
        let raw = self
            .connection
            .query_row(
                "SELECT f.id, f.size_bytes, f.mtime_ns, f.content_hash, f.kind,
                        f.generation_completed
                 FROM files f WHERE f.absolute_path = ?1 AND f.deleted = 0",
                params![absolute_path],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .optional()?;

        raw.map(
            |(id, size_bytes, mtime_ns, content_hash, kind, generation_completed)| {
                Ok(ExistingFile {
                    id,
                    size_bytes: u64::try_from(size_bytes).context("negative catalog file size")?,
                    mtime_ns,
                    content_hash,
                    kind: FileKind::try_from(kind.as_str())?,
                    generation_completed,
                })
            },
        )
        .transpose()
    }

    pub(crate) fn mark_seen(
        &self,
        file_id: i64,
        generation: i64,
        size_bytes: u64,
        mtime_ns: i64,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE files
             SET last_seen_generation = ?1, size_bytes = ?2, mtime_ns = ?3, deleted = 0
             WHERE id = ?4",
            params![
                generation,
                i64::try_from(size_bytes).context("file size exceeds SQLite integer range")?,
                mtime_ns,
                file_id
            ],
        )?;
        Ok(())
    }

    pub(crate) fn mark_seen_only(&self, file_id: i64, generation: i64) -> Result<()> {
        self.connection.execute(
            "UPDATE files SET last_seen_generation = ?1 WHERE id = ?2",
            params![generation, file_id],
        )?;
        Ok(())
    }

    pub(crate) fn mark_path_deleted(
        &self,
        root_id: i64,
        absolute_path: &str,
        generation: i64,
    ) -> Result<bool> {
        let changed = self.connection.execute(
            "UPDATE files
             SET deleted = 1, last_seen_generation = ?1
             WHERE root_id = ?2 AND absolute_path = ?3 AND deleted = 0",
            params![generation, root_id, absolute_path],
        )?;
        Ok(changed > 0)
    }

    pub(crate) fn upsert_file(&self, file: &CatalogFileInput) -> Result<i64> {
        self.connection
            .query_row(
                "INSERT INTO files(
                root_id, absolute_path, relative_path, name, extension, kind, experiment,
                size_bytes, mtime_ns, content_hash, index_generation, last_seen_generation,
                content_indexed, extraction_status, deleted, generation_completed
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12, ?13, 0, 0
             )
             ON CONFLICT(absolute_path) DO UPDATE SET
                root_id = excluded.root_id,
                relative_path = excluded.relative_path,
                name = excluded.name,
                extension = excluded.extension,
                kind = excluded.kind,
                experiment = excluded.experiment,
                size_bytes = excluded.size_bytes,
                mtime_ns = excluded.mtime_ns,
                content_hash = excluded.content_hash,
                index_generation = excluded.index_generation,
                last_seen_generation = excluded.last_seen_generation,
                content_indexed = excluded.content_indexed,
                extraction_status = excluded.extraction_status,
                deleted = 0,
                generation_completed = 0
             RETURNING id",
                params![
                    file.root_id,
                    file.absolute_path,
                    file.relative_path,
                    file.name,
                    file.extension,
                    file.kind.as_str(),
                    file.experiment,
                    i64::try_from(file.size_bytes)
                        .context("file size exceeds SQLite integer range")?,
                    file.mtime_ns,
                    file.content_hash,
                    file.generation,
                    file.content_indexed,
                    file.extraction_status,
                ],
                |row| row.get(0),
            )
            .context("upsert file record")
    }

    pub(crate) fn replace_symbols(&mut self, file_id: i64, symbols: &[SymbolRecord]) -> Result<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute("DELETE FROM symbols WHERE file_id = ?1", params![file_id])?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO symbols(
                    file_id, name, kind, language, line_start, line_end, signature
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for symbol in symbols {
                statement.execute(params![
                    file_id,
                    symbol.name,
                    symbol.kind,
                    symbol.language,
                    i64::try_from(symbol.line_start).context("line_start overflow")?,
                    i64::try_from(symbol.line_end).context("line_end overflow")?,
                    symbol.signature,
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn replace_dataset_profile(
        &self,
        file_id: i64,
        profile: Option<&DatasetProfile>,
    ) -> Result<()> {
        if let Some(profile) = profile {
            self.connection.execute(
                "INSERT INTO dataset_profiles(
                    file_id, format, status, profile_json, profiler, error
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(file_id) DO UPDATE SET
                    format = excluded.format,
                    status = excluded.status,
                    profile_json = excluded.profile_json,
                    profiler = excluded.profiler,
                    error = excluded.error",
                params![
                    file_id,
                    profile.format,
                    profile.status,
                    serde_json::to_string(profile)?,
                    profile.profiler,
                    profile.error,
                ],
            )?;
        } else {
            self.connection.execute(
                "DELETE FROM dataset_profiles WHERE file_id = ?1",
                params![file_id],
            )?;
        }
        Ok(())
    }

    pub(crate) fn replace_agent_document(
        &self,
        file_id: i64,
        metadata: Option<&AgentDocumentMetadata>,
    ) -> Result<()> {
        if let Some(metadata) = metadata {
            self.connection.execute(
                "INSERT INTO agent_documents(
                    file_id, role, name, description, scope_root, precedence_depth,
                    headings_json, references_json
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(file_id) DO UPDATE SET
                    role = excluded.role,
                    name = excluded.name,
                    description = excluded.description,
                    scope_root = excluded.scope_root,
                    precedence_depth = excluded.precedence_depth,
                    headings_json = excluded.headings_json,
                    references_json = excluded.references_json",
                params![
                    file_id,
                    metadata.role.as_str(),
                    metadata.name,
                    metadata.description,
                    metadata.scope_root.to_string_lossy().as_ref(),
                    i64::try_from(metadata.precedence_depth)
                        .context("Agent document precedence depth overflow")?,
                    serde_json::to_string(&metadata.headings)?,
                    serde_json::to_string(&metadata.references)?,
                ],
            )?;
        } else {
            self.connection.execute(
                "DELETE FROM agent_documents WHERE file_id = ?1",
                params![file_id],
            )?;
        }
        Ok(())
    }

    pub(crate) fn replace_agent_memory(
        &self,
        file_id: i64,
        metadata: Option<&AgentMemoryMetadata>,
    ) -> Result<()> {
        if let Some(metadata) = metadata {
            self.connection.execute(
                "INSERT INTO agent_memories(
                    file_id, agent, layer, workspace_root, project_key, session_id,
                    observed_at_ms, raw_history
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(file_id) DO UPDATE SET
                    agent = excluded.agent,
                    layer = excluded.layer,
                    workspace_root = excluded.workspace_root,
                    project_key = excluded.project_key,
                    session_id = excluded.session_id,
                    observed_at_ms = excluded.observed_at_ms,
                    raw_history = excluded.raw_history",
                params![
                    file_id,
                    metadata.agent,
                    metadata.layer.as_str(),
                    metadata
                        .workspace_root
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned()),
                    metadata.project_key,
                    metadata.session_id,
                    metadata.observed_at_ms,
                    metadata.raw_history,
                ],
            )?;
        } else {
            self.connection.execute(
                "DELETE FROM agent_memories WHERE file_id = ?1",
                params![file_id],
            )?;
        }
        Ok(())
    }

    pub(crate) fn record_failure(
        &self,
        generation: i64,
        path: &str,
        stage: &str,
        error: &str,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO index_failures(generation, path, stage, error)
             VALUES (?1, ?2, ?3, ?4)",
            params![generation, path, stage, error],
        )?;
        Ok(())
    }

    pub(crate) fn mark_missing_deleted(
        &mut self,
        root_id: i64,
        generation: i64,
    ) -> Result<Vec<String>> {
        let transaction = self.connection.transaction()?;
        let paths = {
            let mut statement = transaction.prepare(
                "SELECT absolute_path FROM files
                 WHERE root_id = ?1 AND deleted = 0 AND last_seen_generation != ?2",
            )?;
            statement
                .query_map(params![root_id, generation], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        };
        transaction.execute(
            "UPDATE files SET deleted = 1
             WHERE root_id = ?1 AND deleted = 0 AND last_seen_generation != ?2",
            params![root_id, generation],
        )?;
        transaction.commit()?;
        Ok(paths)
    }

    pub(crate) fn latest_completed_generation(&self) -> Result<Option<i64>> {
        self.connection
            .query_row(
                "SELECT MAX(id) FROM generations WHERE status = 'completed'",
                [],
                |row| row.get(0),
            )
            .context("read latest completed generation")
    }

    /// Delete generation bookkeeping rows strictly older than `keep_from`,
    /// retaining every row at or above it. Safe because file lookups compare
    /// generation numbers directly and never join the generations table; the
    /// only consumer of this table is the latest-completed watermark. Returns
    /// the number of rows removed.
    pub(crate) fn prune_generation_rows(&mut self, keep_from: i64) -> Result<usize> {
        let removed = self
            .connection
            .execute("DELETE FROM generations WHERE id < ?1", params![keep_from])?;
        Ok(removed)
    }

    pub(crate) fn checkpoint(&self) -> Result<()> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .context("checkpoint AWI catalog")
    }

    pub(crate) fn current_files_by_ids(
        &self,
        file_ids: &[i64],
    ) -> Result<HashMap<i64, FileRecord>> {
        let Some(generation) = self.latest_completed_generation()? else {
            return Ok(HashMap::new());
        };
        if file_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat_n("?", file_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT
                f.id, r.path, f.absolute_path, f.relative_path, f.name, f.extension,
                f.kind, f.experiment, f.size_bytes, f.mtime_ns, f.content_hash,
                f.index_generation, f.content_indexed, f.extraction_status
             FROM files f JOIN roots r ON r.id = f.root_id
             WHERE f.id IN ({placeholders}) AND f.deleted = 0
               AND f.index_generation <= ? AND f.generation_completed = 1"
        );
        let mut parameters = file_ids.to_vec();
        parameters.push(generation);
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(parameters), raw_file_from_row)?;
        let mut files = HashMap::with_capacity(file_ids.len());
        for row in rows {
            let file: FileRecord = row?.try_into()?;
            files.insert(file.id, file);
        }
        Ok(files)
    }

    pub(crate) fn current_semantic_files(&self) -> Result<Vec<FileRecord>> {
        let Some(generation) = self.latest_completed_generation()? else {
            return Ok(Vec::new());
        };
        let mut statement = self.connection.prepare(
            "SELECT
                f.id, r.path, f.absolute_path, f.relative_path, f.name, f.extension,
                f.kind, f.experiment, f.size_bytes, f.mtime_ns, f.content_hash,
                f.index_generation, f.content_indexed, f.extraction_status
             FROM files f JOIN roots r ON r.id = f.root_id
             WHERE f.deleted = 0
               AND f.index_generation <= ?1 AND f.generation_completed = 1
               AND f.kind IN ('source', 'text', 'semi_structured', 'tabular')
               AND f.size_bytes > 0
               AND (
                    f.content_indexed = 1 OR EXISTS(
                        SELECT 1 FROM dataset_profiles d WHERE d.file_id = f.id
                    )
               )
             ORDER BY f.id",
        )?;
        statement
            .query_map(params![generation], raw_file_from_row)?
            .map(|row| row?.try_into())
            .collect()
    }

    pub(crate) fn current_file_by_path(&self, absolute_path: &str) -> Result<Option<FileRecord>> {
        let Some(generation) = self.latest_completed_generation()? else {
            return Ok(None);
        };
        self.query_file(
            "WHERE f.absolute_path = ?1 AND f.deleted = 0
               AND f.index_generation <= ?2 AND f.generation_completed = 1",
            params![absolute_path, generation],
        )
    }

    pub(crate) fn symbols_for(&self, file_id: i64) -> Result<Vec<SymbolRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT name, kind, language, line_start, line_end, signature
             FROM symbols WHERE file_id = ?1
             ORDER BY line_start, line_end, name",
        )?;
        let symbols = statement
            .query_map(params![file_id], |row| {
                Ok(SymbolRecord {
                    name: row.get(0)?,
                    kind: row.get(1)?,
                    language: row.get(2)?,
                    line_start: row.get::<_, i64>(3)? as usize,
                    line_end: row.get::<_, i64>(4)? as usize,
                    signature: row.get(5)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(symbols)
    }

    pub(crate) fn dataset_profile_for(&self, file_id: i64) -> Result<Option<DatasetProfile>> {
        let json = self
            .connection
            .query_row(
                "SELECT profile_json FROM dataset_profiles WHERE file_id = ?1",
                params![file_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        json.map(|value| serde_json::from_str(&value).context("decode dataset profile"))
            .transpose()
    }

    pub(crate) fn agent_document_for(&self, file_id: i64) -> Result<Option<AgentDocumentMetadata>> {
        let raw = self
            .connection
            .query_row(
                "SELECT role, name, description, scope_root, precedence_depth,
                        headings_json, references_json
                 FROM agent_documents WHERE file_id = ?1",
                params![file_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()?;
        raw.map(agent_document_from_raw).transpose()
    }

    pub(crate) fn agent_documents_for(
        &self,
        file_ids: &[i64],
    ) -> Result<HashMap<i64, AgentDocumentMetadata>> {
        if file_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat_n("?", file_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT file_id, role, name, description, scope_root, precedence_depth,
                    headings_json, references_json
             FROM agent_documents WHERE file_id IN ({placeholders})"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(file_ids.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                ),
            ))
        })?;
        let mut documents = HashMap::with_capacity(file_ids.len());
        for row in rows {
            let (file_id, raw) = row?;
            documents.insert(file_id, agent_document_from_raw(raw)?);
        }
        Ok(documents)
    }

    pub(crate) fn agent_memory_for(&self, file_id: i64) -> Result<Option<AgentMemoryMetadata>> {
        let raw = self
            .connection
            .query_row(
                "SELECT agent, layer, workspace_root, project_key, session_id,
                        observed_at_ms, raw_history
                 FROM agent_memories WHERE file_id = ?1",
                params![file_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, bool>(6)?,
                    ))
                },
            )
            .optional()?;
        raw.map(agent_memory_from_raw).transpose()
    }

    pub(crate) fn agent_memories_for(
        &self,
        file_ids: &[i64],
    ) -> Result<HashMap<i64, AgentMemoryMetadata>> {
        if file_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat_n("?", file_ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT file_id, agent, layer, workspace_root, project_key, session_id,
                    observed_at_ms, raw_history
             FROM agent_memories WHERE file_id IN ({placeholders})"
        );
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(file_ids.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, bool>(7)?,
                ),
            ))
        })?;
        let mut memories = HashMap::with_capacity(file_ids.len());
        for row in rows {
            let (file_id, raw) = row?;
            memories.insert(file_id, agent_memory_from_raw(raw)?);
        }
        Ok(memories)
    }

    pub(crate) fn status(&self) -> Result<IndexStatus> {
        Ok(IndexStatus {
            completed_generation: self.latest_completed_generation()?,
            running_generations: self.count("generations", "status = 'running'")?,
            failed_generations: self.count("generations", "status = 'failed'")?,
            active_files: self.count("files", "deleted = 0 AND generation_completed = 1")?,
            deleted_files: self.count("files", "deleted = 1")?,
            symbols: self.count("symbols", "1 = 1")?,
            datasets: self.count("dataset_profiles", "1 = 1")?,
            agent_documents: {
                let count: i64 = self.connection.query_row(
                    "SELECT COUNT(*)
                     FROM agent_documents a JOIN files f ON f.id = a.file_id
                     WHERE f.deleted = 0 AND f.generation_completed = 1",
                    [],
                    |row| row.get(0),
                )?;
                u64::try_from(count).context("negative Agent document count")?
            },
            agent_memories: {
                let count: i64 = self.connection.query_row(
                    "SELECT COUNT(*)
                     FROM agent_memories m JOIN files f ON f.id = m.file_id
                     WHERE f.deleted = 0 AND f.generation_completed = 1",
                    [],
                    |row| row.get(0),
                )?;
                u64::try_from(count).context("negative Agent memory count")?
            },
            failures: self.count("index_failures", "1 = 1")?,
            semantic: Default::default(),
        })
    }

    fn count(&self, table: &str, predicate: &str) -> Result<u64> {
        let sql = format!("SELECT COUNT(*) FROM {table} WHERE {predicate}");
        let count: i64 = self.connection.query_row(&sql, [], |row| row.get(0))?;
        u64::try_from(count).context("negative SQLite count")
    }

    fn query_file<P>(&self, predicate: &str, parameters: P) -> Result<Option<FileRecord>>
    where
        P: rusqlite::Params,
    {
        let sql = format!(
            "SELECT
                f.id, r.path, f.absolute_path, f.relative_path, f.name, f.extension,
                f.kind, f.experiment, f.size_bytes, f.mtime_ns, f.content_hash,
                f.index_generation, f.content_indexed, f.extraction_status
             FROM files f JOIN roots r ON r.id = f.root_id {predicate}"
        );
        let raw = self
            .connection
            .query_row(&sql, parameters, raw_file_from_row)
            .optional()?;
        raw.map(RawFile::try_into).transpose()
    }
}

struct RawFile {
    id: i64,
    root: String,
    absolute_path: String,
    relative_path: String,
    name: String,
    extension: Option<String>,
    kind: String,
    experiment: Option<String>,
    size_bytes: i64,
    mtime_ns: i64,
    content_hash: Option<String>,
    generation: i64,
    content_indexed: bool,
    extraction_status: String,
}

type RawAgentDocument = (
    String,
    Option<String>,
    Option<String>,
    String,
    i64,
    String,
    String,
);

type RawAgentMemory = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    bool,
);

fn agent_document_from_raw(raw: RawAgentDocument) -> Result<AgentDocumentMetadata> {
    let (role, name, description, scope_root, precedence_depth, headings, references) = raw;
    Ok(AgentDocumentMetadata {
        role: AgentDocumentRole::try_from(role.as_str())?,
        name,
        description,
        scope_root: PathBuf::from(scope_root),
        precedence_depth: usize::try_from(precedence_depth)
            .context("negative Agent document precedence depth")?,
        headings: serde_json::from_str(&headings).context("decode Agent document headings")?,
        references: serde_json::from_str(&references)
            .context("decode Agent document references")?,
    })
}

fn agent_memory_from_raw(raw: RawAgentMemory) -> Result<AgentMemoryMetadata> {
    let (agent, layer, workspace_root, project_key, session_id, observed_at_ms, raw_history) = raw;
    Ok(AgentMemoryMetadata {
        agent,
        layer: AgentMemoryLayer::try_from(layer.as_str())?,
        workspace_root: workspace_root.map(PathBuf::from),
        project_key,
        session_id,
        observed_at_ms,
        raw_history,
    })
}

fn raw_file_from_row(row: &Row<'_>) -> rusqlite::Result<RawFile> {
    Ok(RawFile {
        id: row.get(0)?,
        root: row.get(1)?,
        absolute_path: row.get(2)?,
        relative_path: row.get(3)?,
        name: row.get(4)?,
        extension: row.get(5)?,
        kind: row.get(6)?,
        experiment: row.get(7)?,
        size_bytes: row.get(8)?,
        mtime_ns: row.get(9)?,
        content_hash: row.get(10)?,
        generation: row.get(11)?,
        content_indexed: row.get(12)?,
        extraction_status: row.get(13)?,
    })
}

impl TryFrom<RawFile> for FileRecord {
    type Error = anyhow::Error;

    fn try_from(raw: RawFile) -> Result<Self> {
        Ok(Self {
            id: raw.id,
            root: raw.root,
            absolute_path: raw.absolute_path,
            relative_path: raw.relative_path,
            name: raw.name,
            extension: raw.extension,
            kind: FileKind::try_from(raw.kind.as_str())?,
            experiment: raw.experiment,
            size_bytes: u64::try_from(raw.size_bytes).context("negative catalog file size")?,
            mtime_ns: raw.mtime_ns,
            content_hash: raw.content_hash,
            generation: raw.generation,
            content_indexed: raw.content_indexed,
            extraction_status: raw.extraction_status,
        })
    }
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == column);
    if !exists {
        connection.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
        ))?;
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}
