use super::persistence::{atomic_write, create_private_directory, sync_directory};
use super::DataSchemaError;
use remagic_core::AppId;
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

mod copy;
mod metadata;
mod restore;

use copy::{capture_source, inspect_tree};
use restore::{remove_path, restore_source};

const BACKUP_FORMAT: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
static NEXT_STAGING: AtomicU64 = AtomicU64::new(1);

pub(super) struct BackupStore {
    root: PathBuf,
    app_id: AppId,
}

impl BackupStore {
    pub(super) fn new(root: PathBuf, app_id: AppId) -> Self {
        Self { root, app_id }
    }

    pub(super) fn load_named(&self, name: &str) -> Result<Snapshot, DataSchemaError> {
        if !safe_snapshot_name(name) {
            return Err(DataSchemaError::InvalidBackup(
                "pending transaction has an unsafe snapshot name".into(),
            ));
        }
        let path = self.root.join(name);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| DataSchemaError::io("inspect pending schema backup", &path, error))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(DataSchemaError::InvalidBackup(format!(
                "pending snapshot is not a real directory: {}",
                path.display()
            )));
        }
        Snapshot::load(path, &self.app_id)
    }

    pub(super) fn snapshot(
        &self,
        from_version: Option<u32>,
        to_version: u32,
        sources: &[PathBuf],
    ) -> Result<Snapshot, DataSchemaError> {
        self.remove_staging_directories()?;
        let name = transaction_name(from_version, to_version);
        let final_path = self.root.join(&name);
        self.remove_orphan_transaction(&final_path, from_version, to_version)?;

        let staging = self.root.join(format!(
            ".staging-{name}-{}-{}",
            std::process::id(),
            NEXT_STAGING.fetch_add(1, Ordering::Relaxed)
        ));
        create_private_directory(&staging)?;
        let result = (|| {
            let sources_root = staging.join("sources");
            create_private_directory(&sources_root)?;
            let mut captured = Vec::with_capacity(sources.len());
            for (index, source) in sources.iter().enumerate() {
                let destination = sources_root.join(format!("{index:04}"));
                captured.push(capture_source(source, &destination)?);
            }
            let manifest = BackupManifest {
                format: BACKUP_FORMAT,
                app_id: self.app_id.clone(),
                from_version,
                to_version,
                sources: captured,
            };
            atomic_write(
                &staging.join("backup.json"),
                &serde_json::to_vec(&manifest)?,
            )?;
            sync_directory(&sources_root)?;
            sync_directory(&staging)?;
            let snapshot = Snapshot {
                root: staging.clone(),
                name: name.clone(),
                manifest,
            };
            snapshot.verify()?;
            fs::rename(&staging, &final_path).map_err(|error| {
                DataSchemaError::io("publish schema backup", &final_path, error)
            })?;
            sync_directory(&self.root)?;
            Snapshot::load(final_path, &self.app_id)
        })();
        if result.is_err() {
            let _ = remove_path(&staging);
        }
        result
    }

    fn remove_staging_directories(&self) -> Result<(), DataSchemaError> {
        for entry in fs::read_dir(&self.root)
            .map_err(|error| DataSchemaError::io("list schema backups", &self.root, error))?
        {
            let entry = entry.map_err(|error| {
                DataSchemaError::io("read schema backup entry", &self.root, error)
            })?;
            if entry.file_name().as_bytes().starts_with(b".staging-") {
                remove_path(&entry.path())?;
            }
        }
        Ok(())
    }

    fn remove_orphan_transaction(
        &self,
        path: &Path,
        from_version: Option<u32>,
        to_version: u32,
    ) -> Result<(), DataSchemaError> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(DataSchemaError::InvalidBackup(format!(
                    "orphan snapshot is not a real directory: {}",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(DataSchemaError::io(
                    "inspect orphan schema backup",
                    path,
                    error,
                ));
            }
        }
        let snapshot = Snapshot::load(path.to_path_buf(), &self.app_id)?;
        if snapshot.manifest.from_version != from_version
            || snapshot.manifest.to_version != to_version
        {
            return Err(DataSchemaError::InvalidBackup(
                "orphan snapshot identity does not match its transaction name".into(),
            ));
        }
        snapshot.verify()?;
        snapshot.retire()
    }
}

pub(super) struct Snapshot {
    root: PathBuf,
    name: String,
    manifest: BackupManifest,
}

impl Snapshot {
    fn load(root: PathBuf, app_id: &AppId) -> Result<Self, DataSchemaError> {
        let name = root
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| DataSchemaError::InvalidBackup("backup name is not UTF-8".into()))?
            .to_owned();
        let path = root.join("backup.json");
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| DataSchemaError::io("inspect backup manifest", &path, error))?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_MANIFEST_BYTES
        {
            return Err(DataSchemaError::InvalidBackup(format!(
                "unsafe backup manifest: {}",
                path.display()
            )));
        }
        let bytes = fs::read(&path)
            .map_err(|error| DataSchemaError::io("read backup manifest", &path, error))?;
        let manifest: BackupManifest = serde_json::from_slice(&bytes)
            .map_err(|error| DataSchemaError::InvalidBackup(error.to_string()))?;
        if manifest.format != BACKUP_FORMAT || &manifest.app_id != app_id {
            return Err(DataSchemaError::InvalidBackup(
                "backup format or application identity mismatch".into(),
            ));
        }
        Ok(Self {
            root,
            name,
            manifest,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.root
    }

    pub(super) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn validate_identity(
        &self,
        from_version: Option<u32>,
        to_version: u32,
        sources: &[PathBuf],
    ) -> Result<(), DataSchemaError> {
        let recorded: Vec<_> = self
            .manifest
            .sources
            .iter()
            .map(|source| source.path.clone())
            .collect();
        if self.manifest.from_version != from_version
            || self.manifest.to_version != to_version
            || recorded != sources
        {
            return Err(DataSchemaError::InvalidBackup(
                "existing transaction does not match the requested migration".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn verify(&self) -> Result<(), DataSchemaError> {
        for (index, source) in self.manifest.sources.iter().enumerate() {
            let backup = self.root.join("sources").join(format!("{index:04}"));
            if !source.existed {
                if fs::symlink_metadata(&backup).is_ok() {
                    return Err(DataSchemaError::InvalidBackup(format!(
                        "absent source unexpectedly has backup data: {}",
                        source.path.display()
                    )));
                }
                continue;
            }
            let actual = inspect_tree(&backup)?;
            if actual != content_records(&source.entries) {
                return Err(DataSchemaError::InvalidBackup(format!(
                    "backup content verification failed: {}",
                    source.path.display()
                )));
            }
        }
        Ok(())
    }

    pub(super) fn restore(&self) -> Result<(), DataSchemaError> {
        self.verify()?;
        for (index, source) in self.manifest.sources.iter().enumerate() {
            restore_source(
                source,
                &self.root.join("sources").join(format!("{index:04}")),
                index,
            )?;
        }
        Ok(())
    }

    pub(super) fn retire(self) -> Result<(), DataSchemaError> {
        let parent = self.root.parent().ok_or_else(|| {
            DataSchemaError::InvalidBackup("snapshot has no backup-root parent".into())
        })?;
        let retired = (0..128)
            .find_map(|_| {
                let candidate = parent.join(format!(
                    ".retired-{}-{}-{}",
                    self.name,
                    std::process::id(),
                    NEXT_STAGING.fetch_add(1, Ordering::Relaxed)
                ));
                match fs::symlink_metadata(&candidate) {
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(candidate),
                    _ => None,
                }
            })
            .ok_or_else(|| {
                DataSchemaError::InvalidBackup("could not allocate retired snapshot name".into())
            })?;
        fs::rename(&self.root, &retired)
            .map_err(|error| DataSchemaError::io("retire schema backup", &self.root, error))?;
        sync_directory(parent)?;
        // Retirement itself is the atomic safety boundary. Reclaiming the
        // hidden directory is best effort; a crash or disk error can leave it
        // for later maintenance, but it can never be replayed as a transaction.
        if remove_path(&retired).is_ok() {
            let _ = sync_directory(parent);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BackupManifest {
    format: u32,
    app_id: AppId,
    from_version: Option<u32>,
    to_version: u32,
    sources: Vec<SourceRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceRecord {
    path: PathBuf,
    existed: bool,
    entries: Vec<EntryRecord>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct EntryRecord {
    relative_hex: String,
    kind: EntryKind,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    sha256: Option<String>,
    link_target_hex: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EntryKind {
    Directory,
    File,
    Symlink,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ContentRecord {
    relative_hex: String,
    kind: EntryKind,
    size: u64,
    sha256: Option<String>,
    link_target_hex: Option<String>,
}

fn transaction_name(from: Option<u32>, to: u32) -> String {
    match from {
        Some(from) => format!("from-{from}-to-{to}"),
        None => format!("from-new-to-{to}"),
    }
}

fn safe_snapshot_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
fn content_records(entries: &[EntryRecord]) -> Vec<ContentRecord> {
    entries
        .iter()
        .map(|entry| ContentRecord {
            relative_hex: entry.relative_hex.clone(),
            kind: entry.kind,
            size: entry.size,
            sha256: entry.sha256.clone(),
            link_target_hex: entry.link_target_hex.clone(),
        })
        .collect()
}
