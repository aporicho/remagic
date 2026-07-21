use super::metadata::{
    ensure_same_object, ensure_stable_metadata, entry_record, hex, reject_symlink_ancestors,
};
use super::{ContentRecord, EntryKind, EntryRecord, SourceRecord};
use crate::data_schema::persistence::sync_directory;
use crate::data_schema::DataSchemaError;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(super) fn capture_source(
    source: &Path,
    destination: &Path,
) -> Result<SourceRecord, DataSchemaError> {
    reject_symlink_ancestors(source)?;
    let metadata = match fs::symlink_metadata(source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SourceRecord {
                path: source.to_path_buf(),
                existed: false,
                entries: Vec::new(),
            });
        }
        Err(error) => return Err(DataSchemaError::io("inspect backup source", source, error)),
    };
    let mut entries = Vec::new();
    copy_entry(
        source,
        source,
        destination,
        Path::new(""),
        &metadata,
        &mut entries,
    )?;
    entries.sort_by(|left, right| left.relative_hex.cmp(&right.relative_hex));
    Ok(SourceRecord {
        path: source.to_path_buf(),
        existed: true,
        entries,
    })
}

fn copy_entry(
    access_path: &Path,
    diagnostic_path: &Path,
    destination: &Path,
    relative: &Path,
    observed: &fs::Metadata,
    entries: &mut Vec<EntryRecord>,
) -> Result<(), DataSchemaError> {
    let file_type = observed.file_type();
    if file_type.is_symlink() {
        return copy_symlink_entry(
            access_path,
            diagnostic_path,
            destination,
            relative,
            observed,
            entries,
        );
    }
    if file_type.is_file() {
        let digest = copy_regular_file(access_path, diagnostic_path, destination, observed)?;
        entries.push(entry_record(
            relative,
            EntryKind::File,
            observed,
            observed.len(),
            Some(digest),
            None,
        ));
        return Ok(());
    }
    if file_type.is_dir() {
        return copy_directory_entry(
            access_path,
            diagnostic_path,
            destination,
            relative,
            observed,
            entries,
        );
    }
    Err(DataSchemaError::UnsupportedBackupObject(
        diagnostic_path.to_path_buf(),
    ))
}

fn copy_symlink_entry(
    access_path: &Path,
    diagnostic_path: &Path,
    destination: &Path,
    relative: &Path,
    observed: &fs::Metadata,
    entries: &mut Vec<EntryRecord>,
) -> Result<(), DataSchemaError> {
    let target = fs::read_link(access_path)
        .map_err(|error| DataSchemaError::io("read source symlink", diagnostic_path, error))?;
    symlink(&target, destination)
        .map_err(|error| DataSchemaError::io("copy source symlink", destination, error))?;
    let after = fs::symlink_metadata(access_path)
        .map_err(|error| DataSchemaError::io("verify source symlink", diagnostic_path, error))?;
    ensure_same_object(observed, &after, diagnostic_path)?;
    entries.push(entry_record(
        relative,
        EntryKind::Symlink,
        observed,
        0,
        None,
        Some(hex(target.as_os_str().as_bytes())),
    ));
    Ok(())
}

fn copy_directory_entry(
    access_path: &Path,
    diagnostic_path: &Path,
    destination: &Path,
    relative: &Path,
    observed: &fs::Metadata,
    entries: &mut Vec<EntryRecord>,
) -> Result<(), DataSchemaError> {
    fs::create_dir(destination)
        .map_err(|error| DataSchemaError::io("create backup directory", destination, error))?;
    fs::set_permissions(destination, fs::Permissions::from_mode(0o700))
        .map_err(|error| DataSchemaError::io("secure backup directory", destination, error))?;
    entries.push(entry_record(
        relative,
        EntryKind::Directory,
        observed,
        0,
        None,
        None,
    ));

    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(access_path)
        .map_err(|error| {
            DataSchemaError::io("open backup source directory", diagnostic_path, error)
        })?;
    ensure_same_object(
        observed,
        &directory.metadata().map_err(|error| {
            DataSchemaError::io("inspect open source directory", diagnostic_path, error)
        })?,
        diagnostic_path,
    )?;
    let stable_path = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd()));
    let mut children = fs::read_dir(&stable_path)
        .map_err(|error| {
            DataSchemaError::io("list backup source directory", diagnostic_path, error)
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| {
            DataSchemaError::io("read backup source directory", diagnostic_path, error)
        })?;
    children.sort_by(|left, right| {
        left.file_name()
            .as_bytes()
            .cmp(right.file_name().as_bytes())
    });
    for child in children {
        let name = child.file_name();
        let child_access = stable_path.join(&name);
        let child_diagnostic = diagnostic_path.join(&name);
        let child_destination = destination.join(&name);
        let child_relative = relative.join(&name);
        let metadata = fs::symlink_metadata(&child_access).map_err(|error| {
            DataSchemaError::io("inspect backup source entry", &child_diagnostic, error)
        })?;
        copy_entry(
            &child_access,
            &child_diagnostic,
            &child_destination,
            &child_relative,
            &metadata,
            entries,
        )?;
    }
    let after = directory
        .metadata()
        .map_err(|error| DataSchemaError::io("verify source directory", diagnostic_path, error))?;
    ensure_stable_metadata(observed, &after, diagnostic_path)?;
    sync_directory(destination)
}

fn copy_regular_file(
    source: &Path,
    diagnostic_path: &Path,
    destination: &Path,
    observed: &fs::Metadata,
) -> Result<String, DataSchemaError> {
    let mut input = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(source)
        .map_err(|error| DataSchemaError::io("open backup source file", diagnostic_path, error))?;
    ensure_same_object(
        observed,
        &input.metadata().map_err(|error| {
            DataSchemaError::io("inspect open backup source", diagnostic_path, error)
        })?,
        diagnostic_path,
    )?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(destination)
        .map_err(|error| DataSchemaError::io("create backup file", destination, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input.read(&mut buffer).map_err(|error| {
            DataSchemaError::io("read backup source file", diagnostic_path, error)
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        output
            .write_all(&buffer[..count])
            .map_err(|error| DataSchemaError::io("write backup file", destination, error))?;
    }
    output
        .sync_all()
        .map_err(|error| DataSchemaError::io("sync backup file", destination, error))?;
    let after = input.metadata().map_err(|error| {
        DataSchemaError::io("verify backup source file", diagnostic_path, error)
    })?;
    ensure_stable_metadata(observed, &after, diagnostic_path)?;
    Ok(format!("{:x}", hasher.finalize()))
}
pub(super) fn inspect_tree(root: &Path) -> Result<Vec<ContentRecord>, DataSchemaError> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| DataSchemaError::io("inspect backed-up object", root, error))?;
    let mut records = Vec::new();
    inspect_entry(root, Path::new(""), &metadata, &mut records)?;
    records.sort_by(|left, right| left.relative_hex.cmp(&right.relative_hex));
    Ok(records)
}

fn inspect_entry(
    path: &Path,
    relative: &Path,
    metadata: &fs::Metadata,
    records: &mut Vec<ContentRecord>,
) -> Result<(), DataSchemaError> {
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path)
            .map_err(|error| DataSchemaError::io("verify backup symlink", path, error))?;
        records.push(ContentRecord {
            relative_hex: hex(relative.as_os_str().as_bytes()),
            kind: EntryKind::Symlink,
            size: 0,
            sha256: None,
            link_target_hex: Some(hex(target.as_os_str().as_bytes())),
        });
    } else if metadata.is_file() {
        records.push(ContentRecord {
            relative_hex: hex(relative.as_os_str().as_bytes()),
            kind: EntryKind::File,
            size: metadata.len(),
            sha256: Some(hash_file(path)?),
            link_target_hex: None,
        });
    } else if metadata.is_dir() {
        records.push(ContentRecord {
            relative_hex: hex(relative.as_os_str().as_bytes()),
            kind: EntryKind::Directory,
            size: 0,
            sha256: None,
            link_target_hex: None,
        });
        let mut children = fs::read_dir(path)
            .map_err(|error| DataSchemaError::io("verify backup directory", path, error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| DataSchemaError::io("read backup directory", path, error))?;
        children.sort_by(|left, right| {
            left.file_name()
                .as_bytes()
                .cmp(right.file_name().as_bytes())
        });
        for child in children {
            let name = child.file_name();
            let child_path = child.path();
            let child_metadata = fs::symlink_metadata(&child_path)
                .map_err(|error| DataSchemaError::io("inspect backup entry", &child_path, error))?;
            inspect_entry(&child_path, &relative.join(name), &child_metadata, records)?;
        }
    } else {
        return Err(DataSchemaError::UnsupportedBackupObject(path.to_path_buf()));
    }
    Ok(())
}
pub(super) fn clone_backup_entry(source: &Path, destination: &Path) -> Result<(), DataSchemaError> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| DataSchemaError::io("inspect backup for restore", source, error))?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source)
            .map_err(|error| DataSchemaError::io("read backup symlink", source, error))?;
        return symlink(target, destination)
            .map_err(|error| DataSchemaError::io("restore symlink", destination, error));
    }
    if metadata.is_file() {
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(source)
            .map_err(|error| DataSchemaError::io("open backup for restore", source, error))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(destination)
            .map_err(|error| DataSchemaError::io("create restored file", destination, error))?;
        std::io::copy(&mut input, &mut output)
            .map_err(|error| DataSchemaError::io("copy restored file", destination, error))?;
        return output
            .sync_all()
            .map_err(|error| DataSchemaError::io("sync restored file", destination, error));
    }
    if metadata.is_dir() {
        fs::create_dir(destination).map_err(|error| {
            DataSchemaError::io("create restored directory", destination, error)
        })?;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o700)).map_err(|error| {
            DataSchemaError::io("secure restored directory", destination, error)
        })?;
        let mut children = fs::read_dir(source)
            .map_err(|error| DataSchemaError::io("list backup for restore", source, error))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| DataSchemaError::io("read backup for restore", source, error))?;
        children.sort_by(|left, right| {
            left.file_name()
                .as_bytes()
                .cmp(right.file_name().as_bytes())
        });
        for child in children {
            clone_backup_entry(&child.path(), &destination.join(child.file_name()))?;
        }
        return sync_directory(destination);
    }
    Err(DataSchemaError::UnsupportedBackupObject(
        source.to_path_buf(),
    ))
}
fn hash_file(path: &Path) -> Result<String, DataSchemaError> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| DataSchemaError::io("open backup for verification", path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| DataSchemaError::io("read backup for verification", path, error))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
