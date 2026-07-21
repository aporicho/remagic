use super::{EntryKind, EntryRecord};
use crate::data_schema::persistence::sync_directory;
use crate::data_schema::DataSchemaError;
use std::ffi::{CString, OsString};
use std::fs::{self, OpenOptions};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(super) fn entry_record(
    relative: &Path,
    kind: EntryKind,
    metadata: &fs::Metadata,
    size: u64,
    sha256: Option<String>,
    link_target_hex: Option<String>,
) -> EntryRecord {
    EntryRecord {
        relative_hex: hex(relative.as_os_str().as_bytes()),
        kind,
        mode: metadata.permissions().mode() & 0o7777,
        uid: metadata.uid(),
        gid: metadata.gid(),
        size,
        sha256,
        link_target_hex,
    }
}
pub(super) fn apply_source_metadata(
    root: &Path,
    entries: &[EntryRecord],
) -> Result<(), DataSchemaError> {
    for entry in entries {
        let path = root.join(decode_relative(&entry.relative_hex)?);
        set_owner(&path, entry.uid, entry.gid)?;
        if entry.kind == EntryKind::File {
            fs::set_permissions(&path, fs::Permissions::from_mode(entry.mode))
                .map_err(|error| DataSchemaError::io("restore file permissions", &path, error))?;
            sync_regular_file(&path)?;
        }
    }
    let mut directories: Vec<_> = entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::Directory)
        .collect();
    directories.sort_by_key(|entry| std::cmp::Reverse(entry.relative_hex.len()));
    for entry in directories {
        let path = root.join(decode_relative(&entry.relative_hex)?);
        fs::set_permissions(&path, fs::Permissions::from_mode(entry.mode))
            .map_err(|error| DataSchemaError::io("restore directory permissions", &path, error))?;
        sync_directory(&path)?;
    }
    Ok(())
}

fn sync_regular_file(path: &Path) -> Result<(), DataSchemaError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| DataSchemaError::io("sync restored file metadata", path, error))
}

fn set_owner(path: &Path, uid: u32, gid: u32) -> Result<(), DataSchemaError> {
    let path_bytes = path.as_os_str().as_bytes();
    let c_path = CString::new(path_bytes)
        .map_err(|_| DataSchemaError::InvalidBackup("path contains NUL".into()))?;
    if unsafe { libc::lchown(c_path.as_ptr(), uid, gid) } == 0 {
        Ok(())
    } else {
        Err(DataSchemaError::io(
            "restore data ownership",
            path,
            std::io::Error::last_os_error(),
        ))
    }
}
pub(super) fn reject_symlink_ancestors(path: &Path) -> Result<(), DataSchemaError> {
    let mut ancestors = path.ancestors();
    let _ = ancestors.next();
    for ancestor in ancestors {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(DataSchemaError::InvalidBackup(format!(
                    "backup source traverses a symbolic link: {}",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(DataSchemaError::io(
                    "inspect backup source ancestor",
                    ancestor,
                    error,
                ));
            }
        }
    }
    Ok(())
}

pub(super) fn ensure_same_object(
    before: &fs::Metadata,
    after: &fs::Metadata,
    path: &Path,
) -> Result<(), DataSchemaError> {
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.file_type() != after.file_type()
    {
        return Err(DataSchemaError::SourceChanged(path.to_path_buf()));
    }
    Ok(())
}

pub(super) fn ensure_stable_metadata(
    before: &fs::Metadata,
    after: &fs::Metadata,
    path: &Path,
) -> Result<(), DataSchemaError> {
    ensure_same_object(before, after, path)?;
    if before.len() != after.len()
        || before.mtime() != after.mtime()
        || before.mtime_nsec() != after.mtime_nsec()
        || before.ctime() != after.ctime()
        || before.ctime_nsec() != after.ctime_nsec()
    {
        return Err(DataSchemaError::SourceChanged(path.to_path_buf()));
    }
    Ok(())
}
fn decode_relative(value: &str) -> Result<PathBuf, DataSchemaError> {
    let bytes = unhex(value)?;
    let path = PathBuf::from(OsString::from_vec(bytes));
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(DataSchemaError::InvalidBackup(
            "backup entry escapes its source root".into(),
        ));
    }
    Ok(path)
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn unhex(value: &str) -> Result<Vec<u8>, DataSchemaError> {
    if !value.len().is_multiple_of(2) {
        return Err(DataSchemaError::InvalidBackup(
            "backup path has invalid hex encoding".into(),
        ));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = from_hex_digit(pair[0])?;
            let low = from_hex_digit(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

fn from_hex_digit(value: u8) -> Result<u8, DataSchemaError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(DataSchemaError::InvalidBackup(
            "backup path has invalid hex encoding".into(),
        )),
    }
}
