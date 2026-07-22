use crate::PackageError;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{symlink, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub(crate) fn unique_path(parent: &Path, prefix: &str) -> Result<PathBuf, PackageError> {
    fs::create_dir_all(parent).map_err(|source| PackageError::Io(parent.into(), source))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(parent.join(format!(".{prefix}-{}-{nonce}", std::process::id())))
}

pub(crate) fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), PackageError> {
    let parent = path
        .parent()
        .ok_or_else(|| PackageError::UnsafePath(path.into()))?;
    fs::create_dir_all(parent).map_err(|source| PackageError::Io(parent.into(), source))?;
    let temporary = unique_path(parent, "write")?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .open(&temporary)
            .map_err(|source| PackageError::Io(temporary.clone(), source))?;
        file.write_all(bytes)
            .map_err(|source| PackageError::Io(temporary.clone(), source))?;
        // Callers specify the publication contract, while remagicd itself is
        // deliberately hardened with UMask=0077. Do not let that ambient
        // process setting silently change manifest or state-file modes.
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|source| PackageError::Io(temporary.clone(), source))?;
        file.sync_all()
            .map_err(|source| PackageError::Io(temporary.clone(), source))?;
        fs::rename(&temporary, path).map_err(|source| PackageError::Io(path.into(), source))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn atomic_symlink(target: &Path, link: &Path) -> Result<(), PackageError> {
    let parent = link
        .parent()
        .ok_or_else(|| PackageError::UnsafePath(link.into()))?;
    fs::create_dir_all(parent).map_err(|source| PackageError::Io(parent.into(), source))?;
    let temporary = unique_path(parent, "link")?;
    symlink(target, &temporary).map_err(|source| PackageError::Io(temporary.clone(), source))?;
    fs::rename(&temporary, link).map_err(|source| PackageError::Io(link.into(), source))?;
    sync_directory(parent)
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, PackageError> {
    let mut file = File::open(path).map_err(|source| PackageError::Io(path.into(), source))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|source| PackageError::Io(path.into(), source))?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

pub(crate) fn make_read_only(root: &Path) -> Result<(), PackageError> {
    fn visit(path: &Path) -> Result<(), PackageError> {
        let metadata =
            fs::symlink_metadata(path).map_err(|source| PackageError::Io(path.into(), source))?;
        if metadata.file_type().is_symlink() {
            return Err(PackageError::UnsafePath(path.into()));
        }
        if metadata.is_dir() {
            for entry in
                fs::read_dir(path).map_err(|source| PackageError::Io(path.into(), source))?
            {
                let entry = entry.map_err(|source| PackageError::Io(path.into(), source))?;
                visit(&entry.path())?;
            }
            fs::set_permissions(path, fs::Permissions::from_mode(0o555))
                .map_err(|source| PackageError::Io(path.into(), source))?;
        } else if metadata.is_file() {
            let current = metadata.permissions().mode();
            let mode = if current & 0o111 != 0 { 0o555 } else { 0o444 };
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
                .map_err(|source| PackageError::Io(path.into(), source))?;
        } else {
            return Err(PackageError::UnsafePath(path.into()));
        }
        Ok(())
    }
    visit(root)
}

pub(crate) fn remove_tree(path: &Path) -> Result<(), PackageError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            make_writable(path)?;
            fs::remove_dir_all(path).map_err(|source| PackageError::Io(path.into(), source))
        }
        Ok(_) => fs::remove_file(path).map_err(|source| PackageError::Io(path.into(), source)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(PackageError::Io(path.into(), source)),
    }
}

fn make_writable(path: &Path) -> Result<(), PackageError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| PackageError::Io(path.into(), source))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|source| PackageError::Io(path.into(), source))?;
        for entry in fs::read_dir(path).map_err(|source| PackageError::Io(path.into(), source))? {
            let entry = entry.map_err(|source| PackageError::Io(path.into(), source))?;
            make_writable(&entry.path())?;
        }
    } else if metadata.is_file() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o644))
            .map_err(|source| PackageError::Io(path.into(), source))?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), PackageError> {
    let directory = File::open(path).map_err(|source| PackageError::Io(path.into(), source))?;
    directory
        .sync_all()
        .map_err(|source| PackageError::Io(path.into(), source))
}
