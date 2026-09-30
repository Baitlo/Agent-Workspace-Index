//! Independently published shards, activated as one immutable reader descriptor.
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};

use crate::snapshot::{
    ActivatedSnapshot, SnapshotPointer, materialize_pointer, read_pointer, write_json_sync,
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Visibility {
    #[serde(default)]
    pub exclude_paths: Vec<PathBuf>,
    #[serde(default)]
    pub exclude_kinds: Vec<String>,
}

impl Visibility {
    pub(crate) fn allows(&self, path: &Path, kind: &str) -> bool {
        !self
            .exclude_paths
            .iter()
            .any(|prefix| path.starts_with(prefix))
            && !self.exclude_kinds.iter().any(|excluded| excluded == kind)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShardSource {
    pub name: String,
    pub source: PathBuf,
    #[serde(flatten)]
    pub visibility: Visibility,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub format_version: u32,
    pub shards: Vec<ShardSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocalShard {
    pub definition: ShardSource,
    pub pointer: SnapshotPointer,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocalCollection {
    pub identity: String,
    pub shards: Vec<LocalShard>,
}

/// Compare small pointer documents before touching any large snapshot files.
/// Every pointer is pinned before copying; concurrent publishes are picked up on
/// the next poll. No incomplete set is returned to the daemon.
pub(crate) fn refresh(
    source: &Path,
    cache: &Path,
    previous: Option<&ActivatedSnapshot>,
) -> Result<Option<ActivatedSnapshot>> {
    let bytes = match fs::read(source.join("collection.json")) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let pointer = read_pointer(source)?;
            let identity = fingerprint(&(source, &pointer))?;
            if previous.is_some_and(|active| active.identity == identity) {
                return Ok(None);
            }
            let mut active = materialize_pointer(source, cache, &pointer)?;
            active.identity = identity;
            cleanup(cache, &active.path, previous);
            return Ok(Some(active));
        }
        Err(error) => return Err(error).context("read collection descriptor"),
    };
    let mut collection: Collection =
        serde_json::from_slice(&bytes).context("decode collection descriptor")?;
    if collection.format_version != 1 || collection.shards.is_empty() {
        anyhow::bail!("collection requires format_version=1 and at least one shard");
    }
    let mut names = HashSet::new();
    let mut pinned = Vec::new();
    for definition in &mut collection.shards {
        if definition.name.is_empty()
            || !definition
                .name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
            || !names.insert(definition.name.clone())
        {
            anyhow::bail!("invalid or duplicate shard name: {}", definition.name);
        }
        if !definition.source.is_absolute() {
            definition.source = source.join(&definition.source);
        }
        for path in &definition.visibility.exclude_paths {
            if !path.is_absolute()
                || path
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                anyhow::bail!("shard exclusions must be absolute normalized paths");
            }
        }
        for kind in &definition.visibility.exclude_kinds {
            crate::FileKind::try_from(kind.as_str())?;
        }
        pinned.push(read_pointer(&definition.source)?);
    }
    let identity = fingerprint(&(&collection, &pinned))?;
    if previous.is_some_and(|active| active.identity == identity) {
        return Ok(None);
    }
    let old: Option<LocalCollection> = previous
        .and_then(|active| fs::read(active.path.join("collection.local.json")).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let mut shards = Vec::new();
    for (definition, pointer) in collection.shards.into_iter().zip(pinned) {
        // Only trust caches of the currently active, already verified set.
        let reusable = old.as_ref().and_then(|old| {
            old.shards.iter().find(|shard| {
                shard.definition.name == definition.name
                    && shard.definition.source == definition.source
                    && shard.pointer == pointer
                    && shard.path.is_dir()
            })
        });
        let path = match reusable {
            Some(shard) => shard.path.clone(),
            None => {
                materialize_pointer(
                    &definition.source,
                    &cache.join("shards").join(&definition.name),
                    &pointer,
                )?
                .path
            }
        };
        shards.push(LocalShard {
            definition,
            pointer,
            path,
        });
    }
    let path = cache.join("sets").join(&identity);
    fs::create_dir_all(&path)?;
    let descriptor = LocalCollection {
        identity: identity.clone(),
        shards,
    };
    let temporary = path.join(format!(".collection.tmp-{}", std::process::id()));
    write_json_sync(&temporary, &descriptor)?;
    fs::rename(&temporary, path.join("collection.local.json"))?;
    std::fs::File::open(&path)?.sync_all()?;
    cleanup(cache, &path, previous);
    Ok(Some(ActivatedSnapshot {
        generation: 0,
        path,
        identity,
    }))
}

fn fingerprint(value: &impl Serialize) -> Result<String> {
    Ok(blake3::hash(&serde_json::to_vec(value)?)
        .to_hex()
        .to_string())
}

fn cleanup(cache: &Path, new: &Path, previous: Option<&ActivatedSnapshot>) {
    if let Err(error) = prune_cache(cache, new, previous) {
        eprintln!("AWI reader cache cleanup deferred: {error:#}");
    }
}

/// Keep three recent activations plus all leased readers. A request pins its
/// descriptor for its entire lifetime; GC never removes an in-flight version.
fn prune_cache(cache: &Path, new: &Path, previous: Option<&ActivatedSnapshot>) -> Result<()> {
    let collection = cache.join("sets").is_dir();
    let directory = cache.join(if collection { "sets" } else { "generations" });
    let mut entries = fs::read_dir(&directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.retain(|entry| {
        entry.file_type().is_ok_and(|kind| kind.is_dir())
            && !entry.file_name().to_string_lossy().starts_with('.')
    });
    entries.sort_by_key(|entry| {
        std::cmp::Reverse(
            entry
                .path()
                .join(if collection {
                    "collection.local.json"
                } else {
                    "manifest.json"
                })
                .metadata()
                .and_then(|meta| meta.modified())
                .ok(),
        )
    });
    let mut retained_shards = HashSet::new();
    for (rank, entry) in entries.into_iter().enumerate() {
        let path = entry.path();
        let lease = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.join(".reader.lock"))?;
        let protected =
            rank < 3 || path == new || previous.is_some_and(|active| active.path == path);
        if !protected && lease.try_lock_exclusive()? {
            fs::remove_dir_all(&path)?;
        } else if collection {
            let descriptor: LocalCollection =
                serde_json::from_slice(&fs::read(path.join("collection.local.json"))?)?;
            retained_shards.extend(descriptor.shards.into_iter().map(|shard| shard.path));
        }
    }
    if collection {
        for shard in fs::read_dir(cache.join("shards"))? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for generation in fs::read_dir(shard.path().join("generations"))? {
                let generation = generation?;
                if generation.file_type()?.is_dir()
                    && !generation.file_name().to_string_lossy().starts_with('.')
                    && !retained_shards.contains(&generation.path())
                {
                    fs::remove_dir_all(generation.path())?;
                }
            }
        }
    }
    Ok(())
}
