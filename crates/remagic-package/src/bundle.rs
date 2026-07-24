use crate::filesystem::{remove_tree, safe_relative, sha256_file, unique_path};
use crate::{PackageError, PackagePaths};
use flate2::read::GzDecoder;
use remagic_core::{AppManifest, DeviceProfile, REMAGIC_APP_API_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

mod installed;
pub(crate) use installed::verify_installed_release;

pub const PACKAGE_SCHEMA_V1: u32 = 1;
const MAX_FILES: usize = 100_000;
const MAX_EXPANDED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BundleFileV1 {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    /// Four-digit octal mode, for example `0644` or `0755`.
    pub mode: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BundleV1 {
    pub schema: u32,
    pub app_id: String,
    pub package: String,
    pub version: String,
    pub content_id: String,
    pub manifest_path: String,
    pub payload_sha256: String,
    pub files: Vec<BundleFileV1>,
}

#[derive(Debug)]
pub struct PreparedPackage {
    pub bundle: BundleV1,
    pub manifest: AppManifest,
    pub(crate) manifest_bytes: Vec<u8>,
    pub(crate) stage_root: PathBuf,
}

impl PreparedPackage {
    pub fn app_id(&self) -> &remagic_core::AppId {
        &self.manifest.id
    }
}

impl Drop for PreparedPackage {
    fn drop(&mut self) {
        let _ = remove_tree(&self.stage_root);
    }
}

pub(crate) fn prepare(
    archive: &Path,
    paths: &PackagePaths,
    device: &DeviceProfile,
) -> Result<PreparedPackage, PackageError> {
    device
        .validate()
        .map_err(|error| PackageError::Compatibility(error.to_string()))?;
    let stage_root = unique_path(&paths.staging_root, "package")?;
    fs::create_dir(&stage_root).map_err(|source| PackageError::Io(stage_root.clone(), source))?;
    let result = prepare_at(archive, stage_root.clone(), device);
    if result.is_err() {
        let _ = remove_tree(&stage_root);
    }
    result
}

fn prepare_at(
    archive: &Path,
    stage_root: PathBuf,
    device: &DeviceProfile,
) -> Result<PreparedPackage, PackageError> {
    let input =
        fs::File::open(archive).map_err(|source| PackageError::Io(archive.into(), source))?;
    let decoder = GzDecoder::new(input);
    let mut archive_reader = tar::Archive::new(decoder);
    let mut seen = BTreeSet::new();
    let mut file_count = 0_usize;
    let mut expanded = 0_u64;
    for item in archive_reader
        .entries()
        .map_err(|error| PackageError::Archive(error.to_string()))?
    {
        let mut entry = item.map_err(|error| PackageError::Archive(error.to_string()))?;
        let relative = entry
            .path()
            .map_err(|error| PackageError::Archive(error.to_string()))?
            .into_owned();
        if !safe_relative(&relative) || !seen.insert(relative.clone()) {
            return Err(PackageError::UnsafePath(relative));
        }
        let entry_type = entry.header().entry_type();
        let destination = stage_root.join(&relative);
        if entry_type.is_dir() {
            fs::create_dir_all(&destination)
                .map_err(|source| PackageError::Io(destination.clone(), source))?;
            continue;
        }
        if !entry_type.is_file() {
            return Err(PackageError::UnsupportedEntry(relative));
        }
        file_count += 1;
        expanded = expanded
            .checked_add(entry.size())
            .ok_or(PackageError::ArchiveLimit)?;
        if file_count > MAX_FILES || expanded > MAX_EXPANDED_BYTES {
            return Err(PackageError::ArchiveLimit);
        }
        let mode = entry
            .header()
            .mode()
            .map_err(|error| PackageError::Archive(error.to_string()))?
            & 0o7777;
        if mode & 0o7022 != 0 || mode & 0o400 == 0 {
            return Err(PackageError::UnsafeMode(relative, mode));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| PackageError::UnsafePath(destination.clone()))?;
        fs::create_dir_all(parent).map_err(|source| PackageError::Io(parent.into(), source))?;
        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(mode)
            .open(&destination)
            .map_err(|source| PackageError::Io(destination.clone(), source))?;
        std::io::copy(&mut entry, &mut output)
            .map_err(|source| PackageError::Io(destination.clone(), source))?;
        output
            .flush()
            .map_err(|source| PackageError::Io(destination.clone(), source))?;
        // systemd launches remagicd with UMask=0077. OpenOptions::mode is
        // intentionally filtered by that process umask, so restore the exact
        // already-validated inventory mode before hashing the extracted file.
        output
            .set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|source| PackageError::Io(destination.clone(), source))?;
    }

    let bundle_path = stage_root.join("bundle.json");
    let bundle_bytes = read_limited(&bundle_path, 4 * 1024 * 1024)?;
    let bundle: BundleV1 = serde_json::from_slice(&bundle_bytes)
        .map_err(|error| PackageError::Bundle(error.to_string()))?;
    validate_bundle(&bundle)?;
    verify_inventory(&stage_root, &bundle)?;

    let manifest_path = stage_root.join(&bundle.manifest_path);
    let manifest_bytes = read_limited(&manifest_path, 1024 * 1024)?;
    let manifest: AppManifest = toml::from_str(
        std::str::from_utf8(&manifest_bytes)
            .map_err(|error| PackageError::Manifest(error.to_string()))?,
    )
    .map_err(|error| PackageError::Manifest(error.to_string()))?;
    manifest
        .validate()
        .map_err(|error| PackageError::Manifest(error.to_string()))?;
    validate_manifest_contract(&bundle, &manifest, device)?;

    Ok(PreparedPackage {
        bundle,
        manifest,
        manifest_bytes,
        stage_root,
    })
}

fn validate_bundle(bundle: &BundleV1) -> Result<(), PackageError> {
    if bundle.schema != PACKAGE_SCHEMA_V1 {
        return Err(PackageError::Bundle(format!(
            "unsupported package schema {}",
            bundle.schema
        )));
    }
    if !safe_identifier(&bundle.app_id) || !safe_identifier(&bundle.package) {
        return Err(PackageError::Bundle("invalid app or package id".into()));
    }
    if bundle.version.trim().is_empty()
        || !lower_hex_64(&bundle.content_id)
        || !lower_hex_64(&bundle.payload_sha256)
        || bundle.manifest_path != "manifest.toml"
        || bundle.files.is_empty()
    {
        return Err(PackageError::Bundle("invalid package metadata".into()));
    }
    let mut paths = BTreeSet::new();
    for file in &bundle.files {
        let path = Path::new(&file.path);
        if !safe_relative(path)
            || file.path == "bundle.json"
            || !paths.insert(file.path.clone())
            || !lower_hex_64(&file.sha256)
            || parse_mode(&file.mode).is_none()
        {
            return Err(PackageError::Bundle(format!(
                "invalid inventory entry {}",
                file.path
            )));
        }
    }
    if !paths.contains(&bundle.manifest_path) {
        return Err(PackageError::Bundle("manifest is not in inventory".into()));
    }
    Ok(())
}

fn verify_inventory(root: &Path, bundle: &BundleV1) -> Result<(), PackageError> {
    let actual = regular_files(root)?;
    let expected = bundle
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<BTreeSet<_>>();
    let actual_without_bundle = actual
        .keys()
        .filter(|path| path.as_str() != "bundle.json")
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if expected != actual_without_bundle {
        return Err(PackageError::InventoryMismatch);
    }
    let mut payload_hash = Sha256::new();
    let mut content_hash = Sha256::new();
    content_hash.update(b"remagic-bundle-content-v1\0");
    content_hash.update(bundle.app_id.as_bytes());
    content_hash.update(b"\0");
    content_hash.update(bundle.package.as_bytes());
    content_hash.update(b"\0");
    content_hash.update(bundle.version.as_bytes());
    content_hash.update(b"\0");
    let mut ordered = bundle.files.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| left.path.cmp(&right.path));
    for file in ordered {
        let path = root.join(&file.path);
        let metadata =
            fs::metadata(&path).map_err(|source| PackageError::Io(path.clone(), source))?;
        let mode = metadata.permissions().mode() & 0o7777;
        let expected_mode = parse_mode(&file.mode)
            .ok_or_else(|| PackageError::Bundle(format!("invalid mode for {}", file.path)))?;
        if metadata.len() != file.size
            || mode != expected_mode
            || sha256_file(&path)? != file.sha256
        {
            return Err(PackageError::FileMismatch(file.path.clone()));
        }
        let canonical = format!(
            "{}\0{:o}\0{}\0{}\n",
            file.path, mode, file.size, file.sha256
        );
        content_hash.update(canonical.as_bytes());
        if file.path.starts_with("payload/") {
            payload_hash.update(canonical.as_bytes());
        }
    }
    if hex::encode(payload_hash.finalize()) != bundle.payload_sha256 {
        return Err(PackageError::PayloadMismatch);
    }
    if hex::encode(content_hash.finalize()) != bundle.content_id {
        return Err(PackageError::ContentIdMismatch);
    }
    Ok(())
}

fn regular_files(root: &Path) -> Result<BTreeMap<String, PathBuf>, PackageError> {
    fn visit(
        root: &Path,
        current: &Path,
        output: &mut BTreeMap<String, PathBuf>,
    ) -> Result<(), PackageError> {
        for entry in
            fs::read_dir(current).map_err(|source| PackageError::Io(current.into(), source))?
        {
            let entry = entry.map_err(|source| PackageError::Io(current.into(), source))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|source| PackageError::Io(path.clone(), source))?;
            if metadata.file_type().is_symlink() {
                return Err(PackageError::UnsupportedEntry(path));
            }
            if metadata.is_dir() {
                visit(root, &path, output)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|_| PackageError::UnsafePath(path.clone()))?
                    .to_string_lossy()
                    .into_owned();
                output.insert(relative, path);
            } else {
                return Err(PackageError::UnsupportedEntry(path));
            }
        }
        Ok(())
    }
    let mut output = BTreeMap::new();
    visit(root, root, &mut output)?;
    Ok(output)
}

fn validate_manifest_contract(
    bundle: &BundleV1,
    manifest: &AppManifest,
    device: &DeviceProfile,
) -> Result<(), PackageError> {
    if manifest.id.as_str() != bundle.app_id
        || manifest.package.as_deref() != Some(bundle.package.as_str())
        || manifest.version != bundle.version
    {
        return Err(PackageError::Manifest(
            "manifest identity does not match bundle".into(),
        ));
    }
    if manifest.required_remagic_api > REMAGIC_APP_API_VERSION {
        return Err(PackageError::Compatibility(format!(
            "requires ReMagic API {}",
            manifest.required_remagic_api
        )));
    }
    if !manifest.supported_devices.is_empty()
        && !manifest.supported_devices.contains(&device.product)
    {
        return Err(PackageError::Compatibility(format!(
            "unsupported device {:?}",
            device.product
        )));
    }
    if !manifest.supported_os.is_empty() && !manifest.supported_os.contains(&device.os_version) {
        return Err(PackageError::Compatibility(format!(
            "unsupported OS {}",
            device.os_version
        )));
    }
    let device_root = Path::new("/home/root/apps").join(&bundle.app_id);
    for path in [&manifest.exec, &manifest.working_dir] {
        if !path.starts_with(&device_root) {
            return Err(PackageError::Manifest(format!(
                "runtime path escapes application release: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, PackageError> {
    let mut file = fs::File::open(path).map_err(|source| PackageError::Io(path.into(), source))?;
    let metadata = file
        .metadata()
        .map_err(|source| PackageError::Io(path.into(), source))?;
    if metadata.len() > limit {
        return Err(PackageError::ArchiveLimit);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|source| PackageError::Io(path.into(), source))?;
    Ok(bytes)
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn parse_mode(value: &str) -> Option<u32> {
    (value.len() == 4)
        .then(|| u32::from_str_radix(value, 8).ok())
        .flatten()
        .filter(|mode| mode & 0o7022 == 0 && mode & 0o400 != 0)
}
