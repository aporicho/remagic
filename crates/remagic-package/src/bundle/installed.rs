use super::{parse_mode, regular_files, BundleV1};
use crate::filesystem::sha256_file;
use crate::PackageError;
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub(crate) fn verify_installed_release(
    root: &Path,
    verified_stage: &Path,
    bundle: &BundleV1,
) -> Result<(), PackageError> {
    let actual = regular_files(root)?;
    let mut expected = bundle
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<BTreeSet<_>>();
    expected.insert("bundle.json");
    if expected != actual.keys().map(String::as_str).collect() {
        return Err(PackageError::InventoryMismatch);
    }
    for file in &bundle.files {
        let path = root.join(&file.path);
        let metadata =
            fs::metadata(&path).map_err(|source| PackageError::Io(path.clone(), source))?;
        let source_mode = parse_mode(&file.mode)
            .ok_or_else(|| PackageError::Bundle(format!("invalid mode for {}", file.path)))?;
        let installed_mode = if source_mode & 0o111 != 0 {
            0o555
        } else {
            0o444
        };
        if metadata.len() != file.size
            || metadata.permissions().mode() & 0o7777 != installed_mode
            || sha256_file(&path)? != file.sha256
        {
            return Err(PackageError::FileMismatch(file.path.clone()));
        }
    }
    if sha256_file(&root.join("bundle.json"))? != sha256_file(&verified_stage.join("bundle.json"))?
    {
        return Err(PackageError::FileMismatch("bundle.json".into()));
    }
    Ok(())
}
