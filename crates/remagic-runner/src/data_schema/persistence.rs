use super::DataSchemaError;
use remagic_core::{AppId, SCHEMA_READY_FILE};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) const STATE_FORMAT: u32 = 1;
pub(super) const PENDING_FORMAT: u32 = 1;
const MAX_STATE_BYTES: u64 = 64 * 1024;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AppliedSchema {
    pub format: u32,
    pub app_id: AppId,
    pub version: u32,
    pub backup: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PendingMigration {
    pub format: u32,
    pub app_id: AppId,
    pub from_version: Option<u32>,
    pub to_version: u32,
    pub backup: String,
    pub backup_paths: Vec<PathBuf>,
}

pub(super) struct SchemaStateStore {
    root: PathBuf,
    app_id: AppId,
}

impl SchemaStateStore {
    pub(super) fn open(root: &Path, app_id: &AppId) -> Result<Self, DataSchemaError> {
        create_private_directory(root)?;
        let backups = root.join("backups");
        create_private_directory(&backups)?;
        Ok(Self {
            root: root.to_path_buf(),
            app_id: app_id.clone(),
        })
    }

    pub(super) fn backups_root(&self) -> PathBuf {
        self.root.join("backups")
    }

    pub(super) fn try_lock(&self) -> Result<SchemaLock, DataSchemaError> {
        let path = self.root.join("migration.lock");
        let file = open_private_file(&path, true)?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if result == 0 {
            Ok(SchemaLock { _file: file })
        } else {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                Err(DataSchemaError::ConcurrentTransaction)
            } else {
                Err(DataSchemaError::io("lock schema transaction", path, error))
            }
        }
    }

    pub(super) fn read(&self) -> Result<Option<AppliedSchema>, DataSchemaError> {
        let path = self.root.join("state.json");
        let Some(bytes) = read_private_json_file(&path, "schema state")? else {
            return Ok(None);
        };
        let state: AppliedSchema = serde_json::from_slice(&bytes)
            .map_err(|error| DataSchemaError::InvalidState(error.to_string()))?;
        if state.format != STATE_FORMAT
            || state.app_id != self.app_id
            || state.version == 0
            || !safe_backup_name(&state.backup)
        {
            return Err(DataSchemaError::InvalidState(
                "state identity, version, or backup reference is invalid".into(),
            ));
        }
        Ok(Some(state))
    }

    pub(super) fn read_pending(&self) -> Result<Option<PendingMigration>, DataSchemaError> {
        let path = self.root.join("pending.json");
        let Some(bytes) = read_private_json_file(&path, "pending schema transaction")? else {
            return Ok(None);
        };
        let pending: PendingMigration = serde_json::from_slice(&bytes)
            .map_err(|error| DataSchemaError::InvalidState(error.to_string()))?;
        if pending.format != PENDING_FORMAT
            || pending.app_id != self.app_id
            || pending.to_version == 0
            || pending.from_version == Some(0)
            || pending
                .from_version
                .is_some_and(|from| from >= pending.to_version)
            || !safe_backup_name(&pending.backup)
            || pending
                .backup_paths
                .iter()
                .any(|path| !safe_absolute_path(path))
        {
            return Err(DataSchemaError::InvalidState(
                "pending transaction identity, versions, paths, or backup reference are invalid"
                    .into(),
            ));
        }
        Ok(Some(pending))
    }

    pub(super) fn publish_pending(
        &self,
        pending: &PendingMigration,
    ) -> Result<(), DataSchemaError> {
        if pending.format != PENDING_FORMAT
            || pending.app_id != self.app_id
            || pending.to_version == 0
            || pending.from_version == Some(0)
            || pending
                .from_version
                .is_some_and(|from| from >= pending.to_version)
            || !safe_backup_name(&pending.backup)
            || pending
                .backup_paths
                .iter()
                .any(|path| !safe_absolute_path(path))
        {
            return Err(DataSchemaError::InvalidState(
                "refusing to publish inconsistent pending transaction".into(),
            ));
        }
        atomic_write(
            &self.root.join("pending.json"),
            &serde_json::to_vec(pending)?,
        )
    }

    pub(super) fn clear_pending(&self) -> Result<(), DataSchemaError> {
        let path = self.root.join("pending.json");
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(&self.root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(DataSchemaError::io(
                "clear pending schema transaction",
                path,
                error,
            )),
        }
    }

    pub(super) fn publish(&self, state: &AppliedSchema) -> Result<(), DataSchemaError> {
        if state.format != STATE_FORMAT || state.app_id != self.app_id || state.version == 0 {
            return Err(DataSchemaError::InvalidState(
                "refusing to publish inconsistent state".into(),
            ));
        }
        let bytes = serde_json::to_vec(state)?;
        atomic_write(&self.root.join("state.json"), &bytes)
    }

    pub(super) fn publish_ready(&self, version: u32) -> Result<(), DataSchemaError> {
        if version == 0 {
            return Err(DataSchemaError::InvalidState(
                "refusing to publish a zero schema-ready version".into(),
            ));
        }
        atomic_write(
            &self.root.join(SCHEMA_READY_FILE),
            format!("{}:{version}\n", self.app_id).as_bytes(),
        )
    }

    pub(super) fn clear_ready(&self) -> Result<(), DataSchemaError> {
        let path = self.root.join(SCHEMA_READY_FILE);
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(&self.root),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(DataSchemaError::io(
                "invalidate schema-ready fence",
                path,
                error,
            )),
        }
    }
}

fn read_private_json_file(
    path: &Path,
    label: &'static str,
) -> Result<Option<Vec<u8>>, DataSchemaError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(DataSchemaError::io("inspect managed JSON", path, error)),
    };
    validate_private_regular_file(path, &metadata)?;
    if metadata.len() > MAX_STATE_BYTES {
        return Err(DataSchemaError::InvalidState(format!(
            "{label} exceeds 64 KiB"
        )));
    }
    let file = open_private_file(path, false)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| DataSchemaError::io("read managed JSON", path, error))?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(DataSchemaError::InvalidState(format!(
            "{label} exceeds 64 KiB"
        )));
    }
    Ok(Some(bytes))
}

pub(super) struct SchemaLock {
    _file: File,
}

fn safe_backup_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn safe_absolute_path(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
}

pub(super) fn create_private_directory(path: &Path) -> Result<(), DataSchemaError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(DataSchemaError::InvalidState(format!(
                    "managed path is not a real directory: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                builder.create(&current).map_err(|source| {
                    DataSchemaError::io("create managed schema directory", &current, source)
                })?;
                if let Some(parent) = current.parent().filter(|path| !path.as_os_str().is_empty()) {
                    sync_directory(parent)?;
                }
            }
            Err(error) => {
                return Err(DataSchemaError::io(
                    "inspect managed schema directory",
                    &current,
                    error,
                ));
            }
        }
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| DataSchemaError::io("secure managed schema directory", path, error))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| DataSchemaError::io("verify managed schema directory", path, error))?;
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid || metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(DataSchemaError::InvalidState(format!(
            "managed directory has unsafe owner or mode: {}",
            path.display()
        )));
    }
    sync_directory(path)
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), DataSchemaError> {
    let parent = path.parent().ok_or_else(|| {
        DataSchemaError::InvalidState(format!("path has no parent: {}", path.display()))
    })?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| DataSchemaError::InvalidState("state filename is not UTF-8".into()))?;
    let temporary = parent.join(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
            .open(&temporary)
            .map_err(|error| DataSchemaError::io("create temporary state", &temporary, error))?;
        file.write_all(bytes)
            .map_err(|error| DataSchemaError::io("write temporary state", &temporary, error))?;
        file.sync_all()
            .map_err(|error| DataSchemaError::io("sync temporary state", &temporary, error))?;
        fs::rename(&temporary, path)
            .map_err(|error| DataSchemaError::io("publish state", path, error))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(super) fn sync_directory(path: &Path) -> Result<(), DataSchemaError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| DataSchemaError::io("sync directory", path, error))
}

fn open_private_file(path: &Path, create: bool) -> Result<File, DataSchemaError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|error| DataSchemaError::io("open managed schema file", path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| DataSchemaError::io("inspect managed schema file", path, error))?;
    validate_private_regular_file(path, &metadata)?;
    Ok(file)
}

fn validate_private_regular_file(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), DataSchemaError> {
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.is_file()
        || metadata.uid() != effective_uid
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(DataSchemaError::InvalidState(format!(
            "managed file has unsafe type, owner, or mode: {}",
            path.display()
        )));
    }
    Ok(())
}

use std::os::fd::AsRawFd;
