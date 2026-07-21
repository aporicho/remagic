use super::copy::{clone_backup_entry, inspect_tree};
use super::metadata::{apply_source_metadata, reject_symlink_ancestors};
use super::{content_records, SourceRecord};
use crate::data_schema::persistence::sync_directory;
use crate::data_schema::DataSchemaError;
use std::fs;
use std::path::Path;

pub(super) fn restore_source(
    source: &SourceRecord,
    backup: &Path,
    index: usize,
) -> Result<(), DataSchemaError> {
    reject_symlink_ancestors(&source.path)?;
    if !source.existed {
        remove_path(&source.path)?;
        if let Some(parent) = source.path.parent() {
            sync_directory(parent)?;
        }
        return Ok(());
    }
    let parent = source.path.parent().ok_or_else(|| {
        DataSchemaError::InvalidBackup(format!(
            "backup source has no parent: {}",
            source.path.display()
        ))
    })?;
    let staging = parent.join(format!(
        ".remagic-schema-restore-{}-{index}",
        std::process::id()
    ));
    let old = parent.join(format!(
        ".remagic-schema-old-{}-{index}",
        std::process::id()
    ));
    remove_path(&staging)?;
    remove_path(&old)?;
    clone_backup_entry(backup, &staging)?;
    apply_source_metadata(&staging, &source.entries)?;
    if inspect_tree(&staging)? != content_records(&source.entries) {
        let _ = remove_path(&staging);
        return Err(DataSchemaError::InvalidBackup(format!(
            "restored data failed verification: {}",
            source.path.display()
        )));
    }

    let had_current = fs::symlink_metadata(&source.path).is_ok();
    if had_current {
        fs::rename(&source.path, &old).map_err(|error| {
            DataSchemaError::io("stage current application data", &source.path, error)
        })?;
    }
    if let Err(error) = fs::rename(&staging, &source.path) {
        if had_current {
            let _ = fs::rename(&old, &source.path);
        }
        return Err(DataSchemaError::io(
            "publish restored application data",
            &source.path,
            error,
        ));
    }
    sync_directory(parent)?;
    remove_path(&old)?;
    Ok(())
}
pub(super) fn remove_path(path: &Path) -> Result<(), DataSchemaError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
                .map_err(|error| DataSchemaError::io("remove transaction path", path, error))
        }
        Ok(_) => fs::remove_file(path)
            .map_err(|error| DataSchemaError::io("remove transaction path", path, error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DataSchemaError::io("inspect transaction path", path, error)),
    }
}
