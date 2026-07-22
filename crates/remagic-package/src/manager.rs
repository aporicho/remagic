use crate::bundle;
use crate::filesystem::{atomic_symlink, atomic_write, remove_tree};
use crate::state::PACKAGE_STATE_SCHEMA_V1;
use crate::{InstalledPackageStateV1, PreparedPackage};
use remagic_core::{AppId, AppKind, AppManifest, DeviceProfile};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use thiserror::Error;

mod install;
mod manifest;
mod transaction;

pub(crate) use manifest::materialize_manifest_bytes;

#[derive(Clone, Debug)]
pub struct PackagePaths {
    pub apps_root: PathBuf,
    pub manifest_root: PathBuf,
    pub state_root: PathBuf,
    pub staging_root: PathBuf,
    pub books_root: PathBuf,
}

impl Default for PackagePaths {
    fn default() -> Self {
        let apps_root = env_path("REMAGIC_APPS_ROOT", "/home/root/apps");
        Self {
            staging_root: apps_root.join(".staging"),
            apps_root,
            manifest_root: env_path(
                "REMAGIC_MANIFEST_ROOT",
                "/home/root/.local/share/remagic/apps.d",
            ),
            state_root: env_path(
                "REMAGIC_PACKAGE_STATE_ROOT",
                "/home/root/.local/state/remagic/packages",
            ),
            books_root: env_path("REMAGIC_BOOKS_ROOT", "/home/root/books"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PackageManager {
    paths: PackagePaths,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InstallOutcome {
    pub app_id: AppId,
    pub version: String,
    pub content_id: String,
    pub previous_content_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UninstallOutcome {
    pub app_id: AppId,
    pub data_purged: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TransactionJournalV1 {
    schema: u32,
    app_id: String,
    target_content_id: String,
    previous_content_id: Option<String>,
    previous_manifest: Option<String>,
    previous_state: Option<String>,
}

impl PackageManager {
    pub fn new(paths: PackagePaths) -> Self {
        Self { paths }
    }

    pub fn from_environment() -> Self {
        Self::new(PackagePaths::default())
    }

    pub fn paths(&self) -> &PackagePaths {
        &self.paths
    }

    pub fn prepare(
        &self,
        archive: &Path,
        device: &DeviceProfile,
    ) -> Result<PreparedPackage, PackageError> {
        self.recover_all()?;
        bundle::prepare(archive, &self.paths, device)
    }

    pub fn rollback(
        &self,
        app_id: &AppId,
        version: Option<&str>,
        device: &DeviceProfile,
    ) -> Result<InstallOutcome, PackageError> {
        self.recover_for(app_id)?;
        let state = self.read_state(app_id)?;
        let target = match version {
            Some(version) => self.release_for_version(app_id, version)?,
            None => state
                .previous_content_id
                .clone()
                .ok_or_else(|| PackageError::NoRollback(app_id.to_string()))?,
        };
        if target == state.current_content_id {
            return Err(PackageError::NoRollback(app_id.to_string()));
        }
        let release_root = self
            .paths
            .apps_root
            .join(app_id.as_str())
            .join("releases")
            .join(&target);
        let manifest_path_in_release = release_root.join("manifest.toml");
        let bundled_manifest_bytes = fs::read(&manifest_path_in_release)
            .map_err(|source| PackageError::Io(manifest_path_in_release.clone(), source))?;
        let manifest_bytes = materialize_manifest_bytes(&bundled_manifest_bytes, app_id, &target)?;
        let manifest: AppManifest = toml::from_str(
            std::str::from_utf8(&manifest_bytes)
                .map_err(|error| PackageError::Manifest(error.to_string()))?,
        )
        .map_err(|error| PackageError::Manifest(error.to_string()))?;
        manifest
            .validate()
            .map_err(|error| PackageError::Manifest(error.to_string()))?;
        if &manifest.id != app_id
            || (!manifest.supported_devices.is_empty()
                && !manifest.supported_devices.contains(&device.product))
            || (!manifest.supported_os.is_empty()
                && !manifest.supported_os.contains(&device.os_version))
        {
            return Err(PackageError::Compatibility(
                "rollback release is incompatible with this device".into(),
            ));
        }
        let app_root = self.paths.apps_root.join(app_id.as_str());
        atomic_write(&self.manifest_path(app_id), &manifest_bytes, 0o644)?;
        atomic_symlink(
            Path::new("releases").join(&target).as_path(),
            &app_root.join("current"),
        )?;
        let next = InstalledPackageStateV1 {
            schema: PACKAGE_STATE_SCHEMA_V1,
            app_id: app_id.to_string(),
            package: state.package,
            current_content_id: target.clone(),
            previous_content_id: Some(state.current_content_id.clone()),
            version: manifest.version.clone(),
        };
        atomic_write(
            &self.state_path(app_id),
            &serde_json::to_vec_pretty(&next)
                .map_err(|error| PackageError::State(error.to_string()))?,
            0o600,
        )?;
        Ok(InstallOutcome {
            app_id: app_id.clone(),
            version: manifest.version,
            content_id: target,
            previous_content_id: Some(state.current_content_id),
        })
    }

    pub fn uninstall(&self, app_id: &AppId, purge: bool) -> Result<UninstallOutcome, PackageError> {
        self.recover_for(app_id)?;
        let manifest_path = self.manifest_path(app_id);
        let text = fs::read_to_string(&manifest_path)
            .map_err(|source| PackageError::Io(manifest_path.clone(), source))?;
        let manifest: AppManifest =
            toml::from_str(&text).map_err(|error| PackageError::Manifest(error.to_string()))?;
        manifest
            .validate()
            .map_err(|error| PackageError::Manifest(error.to_string()))?;
        if manifest.kind == AppKind::System {
            return Err(PackageError::SystemApp(app_id.to_string()));
        }
        if purge {
            self.purge_application_data(&manifest)?;
        }
        fs::remove_file(&manifest_path)
            .map_err(|source| PackageError::Io(manifest_path.clone(), source))?;
        remove_tree(&self.paths.apps_root.join(app_id.as_str()))?;
        remove_optional_file(&self.state_path(app_id))?;
        Ok(UninstallOutcome {
            app_id: app_id.clone(),
            data_purged: purge,
        })
    }

    pub fn read_state(&self, app_id: &AppId) -> Result<InstalledPackageStateV1, PackageError> {
        let path = self.state_path(app_id);
        let text = fs::read_to_string(&path).map_err(|source| PackageError::Io(path, source))?;
        let state: InstalledPackageStateV1 =
            serde_json::from_str(&text).map_err(|error| PackageError::State(error.to_string()))?;
        if state.schema != PACKAGE_STATE_SCHEMA_V1 || state.app_id != app_id.as_str() {
            return Err(PackageError::State(
                "installed package state mismatch".into(),
            ));
        }
        Ok(state)
    }

    fn purge_application_data(&self, manifest: &AppManifest) -> Result<(), PackageError> {
        let Some(directories) = &manifest.runtime.directories else {
            return Ok(());
        };
        let allowed_roots = [
            Path::new("/home/root/.config"),
            Path::new("/home/root/.local/share"),
            Path::new("/home/root/.local/state"),
            Path::new("/home/root/.cache"),
        ];
        for path in [
            &directories.config_home,
            &directories.data_home,
            &directories.state_home,
            &directories.cache_home,
        ] {
            let allowed = allowed_roots.iter().any(|root| {
                path.starts_with(root) && path != root && !path.starts_with(&self.paths.books_root)
            });
            if !allowed {
                return Err(PackageError::UnsafePurge(path.clone()));
            }
            remove_tree(path)?;
        }
        Ok(())
    }

    fn ensure_application_directories(&self, manifest: &AppManifest) -> Result<(), PackageError> {
        let Some(directories) = &manifest.runtime.directories else {
            return Ok(());
        };
        let allowed_roots = [
            Path::new("/home/root/.config"),
            Path::new("/home/root/.local/share"),
            Path::new("/home/root/.local/state"),
            Path::new("/home/root/.cache"),
        ];
        for path in [
            &directories.config_home,
            &directories.data_home,
            &directories.state_home,
            &directories.cache_home,
        ] {
            let allowed = allowed_roots
                .iter()
                .any(|root| path.starts_with(root) && path != root);
            if !allowed {
                return Err(PackageError::UnsafeDataPath(path.clone()));
            }
            fs::create_dir_all(path).map_err(|source| PackageError::Io(path.clone(), source))?;
            let metadata = fs::symlink_metadata(path)
                .map_err(|source| PackageError::Io(path.clone(), source))?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(PackageError::UnsafeDataPath(path.clone()));
            }
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|source| PackageError::Io(path.clone(), source))?;
        }
        Ok(())
    }

    fn release_for_version(&self, app_id: &AppId, version: &str) -> Result<String, PackageError> {
        let root = self.paths.apps_root.join(app_id.as_str()).join("releases");
        let mut matches = Vec::new();
        for entry in fs::read_dir(&root).map_err(|source| PackageError::Io(root.clone(), source))? {
            let entry = entry.map_err(|source| PackageError::Io(root.clone(), source))?;
            let path = entry.path().join("manifest.toml");
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(manifest) = toml::from_str::<AppManifest>(&text) else {
                continue;
            };
            if manifest.id == *app_id && manifest.version == version {
                matches.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        if matches.len() == 1 {
            Ok(matches.remove(0))
        } else {
            Err(PackageError::NoRollback(format!(
                "{app_id} version {version}"
            )))
        }
    }

    fn remove_stale_releases(
        &self,
        app_id: &AppId,
        state: &InstalledPackageStateV1,
    ) -> Result<(), PackageError> {
        let keep = [
            Some(state.current_content_id.as_str()),
            state.previous_content_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
        let root = self.paths.apps_root.join(app_id.as_str()).join("releases");
        for entry in fs::read_dir(&root).map_err(|source| PackageError::Io(root.clone(), source))? {
            let entry = entry.map_err(|source| PackageError::Io(root.clone(), source))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !keep.contains(name.as_str()) {
                remove_tree(&entry.path())?;
            }
        }
        Ok(())
    }

    fn manifest_path(&self, app_id: &AppId) -> PathBuf {
        self.paths.manifest_root.join(format!("{app_id}.toml"))
    }

    fn state_path(&self, app_id: &AppId) -> PathBuf {
        self.paths.state_root.join(format!("{app_id}.json"))
    }

    fn journal_path(&self, app_id: &AppId) -> PathBuf {
        self.paths.state_root.join(format!("{app_id}.journal"))
    }
}

fn env_path(key: &str, fallback: &str) -> PathBuf {
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback))
}

fn read_optional_text(path: &Path) -> Result<Option<String>, PackageError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PackageError::Io(path.into(), source)),
    }
}

fn remove_optional_file(path: &Path) -> Result<(), PackageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(PackageError::Io(path.into(), source)),
    }
}

fn current_content_id(link: &Path) -> Option<String> {
    let target = fs::read_link(link).ok()?;
    target.file_name()?.to_str().map(str::to_owned)
}

#[derive(Debug, Error)]
pub enum PackageError {
    #[error("I/O error at {0}: {1}")]
    Io(PathBuf, #[source] std::io::Error),
    #[error("invalid package archive: {0}")]
    Archive(String),
    #[error("package exceeds extraction limits")]
    ArchiveLimit,
    #[error("unsafe package path: {0}")]
    UnsafePath(PathBuf),
    #[error("unsupported package entry: {0}")]
    UnsupportedEntry(PathBuf),
    #[error("unsafe mode {1:o} for {0}")]
    UnsafeMode(PathBuf, u32),
    #[error("invalid bundle: {0}")]
    Bundle(String),
    #[error("bundle inventory does not exactly match archive files")]
    InventoryMismatch,
    #[error("bundle file does not match inventory: {0}")]
    FileMismatch(String),
    #[error("payload digest does not match bundle")]
    PayloadMismatch,
    #[error("content id does not match the canonical bundle inventory")]
    ContentIdMismatch,
    #[error("invalid application manifest: {0}")]
    Manifest(String),
    #[error("application is incompatible: {0}")]
    Compatibility(String),
    #[error("package state is invalid: {0}")]
    State(String),
    #[error("release already exists: {0}")]
    ReleaseExists(String),
    #[error("no rollback release for {0}")]
    NoRollback(String),
    #[error("system application cannot be uninstalled: {0}")]
    SystemApp(String),
    #[error("refusing to purge unsafe path: {0}")]
    UnsafePurge(PathBuf),
    #[error("refusing unsafe application data path: {0}")]
    UnsafeDataPath(PathBuf),
}
