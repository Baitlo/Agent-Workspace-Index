use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const SNAPSHOT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub path: String,
    pub size_bytes: u64,
    pub blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotManifest {
    pub format_version: u32,
    pub generation: i64,
    pub created_at_ms: i64,
    pub files: Vec<SnapshotFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotPointer {
    pub format_version: u32,
    pub generation: i64,
    pub directory: String,
    pub manifest_blake3: String,
}

#[derive(Debug, Clone)]
pub struct ActivatedSnapshot {
    pub generation: i64,
    pub path: PathBuf,
}

pub(crate) fn publish(
    index_dir: &Path,
    publish_dir: &Path,
    generation: i64,
) -> Result<SnapshotManifest> {
    let generations_dir = publish_dir.join("generations");
    fs::create_dir_all(&generations_dir).with_context(|| {
        format!(
            "create snapshot generations directory {}",
            generations_dir.display()
        )
    })?;
    let directory = format!("generation-{generation:020}");
    let final_dir = generations_dir.join(&directory);
    if final_dir.exists() {
        let manifest = verify_snapshot(&final_dir)?;
        if manifest.generation != generation {
            anyhow::bail!("snapshot generation mismatch at {}", final_dir.display());
        }
        publish_pointer(publish_dir, &directory, &manifest)?;
        return Ok(manifest);
    }

    let staging = generations_dir.join(format!(".{directory}.tmp-{}", std::process::id()));
    if staging.exists() {
        fs::remove_dir_all(&staging)
            .with_context(|| format!("remove stale snapshot staging {}", staging.display()))?;
    }
    fs::create_dir(&staging)
        .with_context(|| format!("create snapshot staging {}", staging.display()))?;

    let result = (|| {
        copy_tree(
            &index_dir.join("catalog.sqlite3"),
            &staging.join("catalog.sqlite3"),
        )?;
        copy_tree(&index_dir.join("tantivy"), &staging.join("tantivy"))?;
        let files = snapshot_files(&staging)?;
        let manifest = SnapshotManifest {
            format_version: SNAPSHOT_FORMAT_VERSION,
            generation,
            created_at_ms: now_ms(),
            files,
        };
        write_json_sync(&staging.join("manifest.json"), &manifest)?;
        sync_directory(&staging)?;
        make_read_only(&staging)?;
        fs::rename(&staging, &final_dir).with_context(|| {
            format!(
                "publish snapshot {} -> {}",
                staging.display(),
                final_dir.display()
            )
        })?;
        sync_directory(&generations_dir)?;
        publish_pointer(publish_dir, &directory, &manifest)?;
        Ok(manifest)
    })();
    if result.is_err() && staging.exists() {
        let _ = make_writable(&staging);
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

pub(crate) fn materialize_latest(
    publish_dir: &Path,
    cache_dir: &Path,
) -> Result<ActivatedSnapshot> {
    let pointer_path = publish_dir.join("current.json");
    let pointer: SnapshotPointer = serde_json::from_slice(
        &fs::read(&pointer_path)
            .with_context(|| format!("read snapshot pointer {}", pointer_path.display()))?,
    )
    .with_context(|| format!("decode snapshot pointer {}", pointer_path.display()))?;
    if pointer.format_version != SNAPSHOT_FORMAT_VERSION {
        anyhow::bail!(
            "unsupported snapshot pointer version {}",
            pointer.format_version
        );
    }
    validate_directory_name(&pointer.directory)?;

    let source = publish_dir.join("generations").join(&pointer.directory);
    let source_manifest = verify_snapshot(&source)?;
    let manifest_bytes = fs::read(source.join("manifest.json"))?;
    if blake3::hash(&manifest_bytes).to_hex().as_str() != pointer.manifest_blake3 {
        anyhow::bail!(
            "snapshot manifest checksum mismatch at {}",
            source.display()
        );
    }
    if source_manifest.generation != pointer.generation {
        anyhow::bail!("snapshot pointer generation does not match manifest");
    }

    let generations_dir = cache_dir.join("generations");
    fs::create_dir_all(&generations_dir)?;
    let local = generations_dir.join(&pointer.directory);
    if local.exists() {
        if verify_snapshot(&local).is_ok() {
            return Ok(ActivatedSnapshot {
                generation: pointer.generation,
                path: local,
            });
        }
        make_writable(&local)?;
        fs::remove_dir_all(&local)?;
    }

    let staging =
        generations_dir.join(format!(".{}.tmp-{}", pointer.directory, std::process::id()));
    if staging.exists() {
        make_writable(&staging)?;
        fs::remove_dir_all(&staging)?;
    }
    copy_tree(&source, &staging)?;
    verify_snapshot(&staging)?;
    make_writable(&staging)?;
    fs::rename(&staging, &local)?;
    sync_directory(&generations_dir)?;
    Ok(ActivatedSnapshot {
        generation: pointer.generation,
        path: local,
    })
}

pub(crate) fn read_pointer(publish_dir: &Path) -> Result<SnapshotPointer> {
    let path = publish_dir.join("current.json");
    serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("read snapshot pointer {}", path.display()))?,
    )
    .with_context(|| format!("decode snapshot pointer {}", path.display()))
}

pub(crate) fn verify_snapshot(path: &Path) -> Result<SnapshotManifest> {
    let manifest_path = path.join("manifest.json");
    let manifest: SnapshotManifest = serde_json::from_slice(
        &fs::read(&manifest_path)
            .with_context(|| format!("read snapshot manifest {}", manifest_path.display()))?,
    )
    .with_context(|| format!("decode snapshot manifest {}", manifest_path.display()))?;
    if manifest.format_version != SNAPSHOT_FORMAT_VERSION {
        anyhow::bail!(
            "unsupported snapshot manifest version {}",
            manifest.format_version
        );
    }
    for entry in &manifest.files {
        let relative = Path::new(&entry.path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
        {
            anyhow::bail!("unsafe snapshot manifest path: {}", entry.path);
        }
        let file = path.join(relative);
        let metadata = fs::metadata(&file)
            .with_context(|| format!("read snapshot file metadata {}", file.display()))?;
        if metadata.len() != entry.size_bytes {
            anyhow::bail!("snapshot size mismatch for {}", entry.path);
        }
        if hash_file(&file)? != entry.blake3 {
            anyhow::bail!("snapshot checksum mismatch for {}", entry.path);
        }
    }
    Ok(manifest)
}

fn publish_pointer(publish_dir: &Path, directory: &str, manifest: &SnapshotManifest) -> Result<()> {
    let manifest_bytes = serde_json::to_vec_pretty(manifest)?;
    let mut manifest_bytes = manifest_bytes;
    manifest_bytes.push(b'\n');
    let pointer = SnapshotPointer {
        format_version: SNAPSHOT_FORMAT_VERSION,
        generation: manifest.generation,
        directory: directory.to_owned(),
        manifest_blake3: blake3::hash(&manifest_bytes).to_hex().to_string(),
    };
    let temporary = publish_dir.join(format!(".current.tmp-{}", std::process::id()));
    write_json_sync(&temporary, &pointer)?;
    fs::rename(&temporary, publish_dir.join("current.json"))
        .context("atomically publish snapshot pointer")?;
    sync_directory(publish_dir)
}

fn snapshot_files(root: &Path) -> Result<Vec<SnapshotFile>> {
    let mut paths = Vec::new();
    collect_files(root, root, &mut paths)?;
    paths.sort();
    paths
        .into_iter()
        .filter(|path| path != Path::new("manifest.json"))
        .map(|relative| {
            let path = root.join(&relative);
            Ok(SnapshotFile {
                path: relative.to_string_lossy().into_owned(),
                size_bytes: fs::metadata(&path)?.len(),
                blake3: hash_file(&path)?,
            })
        })
        .collect()
}

fn collect_files(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_symlink() {
            anyhow::bail!("snapshot source contains symlink: {}", path.display());
        }
        if file_type.is_dir() {
            collect_files(root, &path, output)?;
        } else if file_type.is_file() {
            output.push(path.strip_prefix(root)?.to_owned());
        }
    }
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let metadata =
        fs::symlink_metadata(source).with_context(|| format!("inspect {}", source.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("refusing to copy snapshot symlink {}", source.display());
    }
    if metadata.is_file() {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, target).with_context(|| {
            format!(
                "copy snapshot file {} -> {}",
                source.display(),
                target.display()
            )
        })?;
        File::open(target)?.sync_all()?;
        return Ok(());
    }
    if !metadata.is_dir() {
        anyhow::bail!("unsupported snapshot entry {}", source.display());
    }
    fs::create_dir_all(target)?;
    let mut entries = fs::read_dir(source)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        copy_tree(&entry.path(), &target.join(entry.file_name()))?;
    }
    sync_directory(target)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut reader = BufReader::new(File::open(path)?);
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn write_json_sync(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn make_read_only(path: &Path) -> Result<()> {
    if path.is_dir() {
        for entry in fs::read_dir(path)? {
            make_read_only(&entry?.path())?;
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o555))?;
    } else {
        fs::set_permissions(path, fs::Permissions::from_mode(0o444))?;
    }
    Ok(())
}

fn make_writable(path: &Path) -> Result<()> {
    if path.is_dir() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        for entry in fs::read_dir(path)? {
            make_writable(&entry?.path())?;
        }
    } else {
        fs::set_permissions(path, fs::Permissions::from_mode(0o644))?;
    }
    Ok(())
}

fn validate_directory_name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.contains('/')
        || value.contains('\\')
        || value == "."
        || value == ".."
    {
        anyhow::bail!("unsafe snapshot directory name: {value}");
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn publishes_verifies_and_materializes_snapshot() {
        let fixture = tempdir().unwrap();
        let index = fixture.path().join("index");
        let publish_dir = fixture.path().join("publish");
        let cache = fixture.path().join("cache");
        fs::create_dir_all(index.join("tantivy")).unwrap();
        fs::write(index.join("catalog.sqlite3"), b"catalog").unwrap();
        fs::write(index.join("tantivy/meta.json"), b"index").unwrap();

        let manifest = publish(&index, &publish_dir, 7).unwrap();
        assert_eq!(manifest.generation, 7);
        assert_eq!(manifest.files.len(), 2);
        let activated = materialize_latest(&publish_dir, &cache).unwrap();
        assert_eq!(activated.generation, 7);
        assert_eq!(
            fs::read(activated.path.join("catalog.sqlite3")).unwrap(),
            b"catalog"
        );
    }
}
