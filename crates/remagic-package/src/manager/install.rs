use super::{
    current_content_id, materialize_manifest_bytes, read_optional_text, InstallOutcome,
    PackageError, PackageManager, TransactionJournalV1,
};
use crate::filesystem::{atomic_symlink, atomic_write, make_read_only, remove_tree};
use crate::{InstalledPackageStateV1, PreparedPackage};
use remagic_core::AppId;
use std::fs;
use std::path::{Path, PathBuf};

struct InstallPaths {
    app_root: PathBuf,
    release_root: PathBuf,
    manifest: PathBuf,
    state: PathBuf,
    journal: PathBuf,
}

impl PackageManager {
    pub fn install(&self, prepared: PreparedPackage) -> Result<InstallOutcome, PackageError> {
        let app_id = prepared.manifest.id.clone();
        self.recover_for(&app_id)?;
        let paths = self.prepare_install_paths(&app_id, &prepared)?;
        if fs::symlink_metadata(&paths.release_root).is_ok() {
            return self.finish_current_reinstall(app_id, &prepared, &paths.app_root);
        }

        let previous_content_id = self.begin_install(&app_id, &prepared, &paths)?;
        let outcome = self.publish_install(&app_id, &prepared, &paths, previous_content_id);
        if outcome.is_err() {
            let _ = self.recover_journal(&paths.journal);
        }
        outcome
    }

    fn prepare_install_paths(
        &self,
        app_id: &AppId,
        prepared: &PreparedPackage,
    ) -> Result<InstallPaths, PackageError> {
        for path in [
            &self.paths.books_root,
            &self.paths.manifest_root,
            &self.paths.state_root,
        ] {
            fs::create_dir_all(path).map_err(|source| PackageError::Io(path.clone(), source))?;
        }
        self.ensure_application_directories(&prepared.manifest)?;
        let app_root = self.paths.apps_root.join(app_id.as_str());
        let releases_root = app_root.join("releases");
        fs::create_dir_all(&releases_root)
            .map_err(|source| PackageError::Io(releases_root.clone(), source))?;
        Ok(InstallPaths {
            release_root: releases_root.join(&prepared.bundle.content_id),
            manifest: self.manifest_path(app_id),
            state: self.state_path(app_id),
            journal: self.journal_path(app_id),
            app_root,
        })
    }

    fn finish_current_reinstall(
        &self,
        app_id: AppId,
        prepared: &PreparedPackage,
        app_root: &Path,
    ) -> Result<InstallOutcome, PackageError> {
        let state = self.read_state(&app_id)?;
        let is_current = state.current_content_id == prepared.bundle.content_id
            && state.version == prepared.bundle.version
            && state.package == prepared.bundle.package
            && current_content_id(&app_root.join("current"))
                == Some(prepared.bundle.content_id.clone());
        if !is_current {
            remove_tree(&prepared.stage_root)?;
            return Err(PackageError::ReleaseExists(
                prepared.bundle.content_id.clone(),
            ));
        }
        let manifest_bytes = materialize_manifest_bytes(
            &prepared.manifest_bytes,
            &app_id,
            &prepared.bundle.content_id,
        )?;
        atomic_write(&self.manifest_path(&app_id), &manifest_bytes, 0o644)?;
        remove_tree(&prepared.stage_root)?;
        Ok(InstallOutcome {
            app_id,
            version: state.version,
            content_id: state.current_content_id,
            previous_content_id: state.previous_content_id,
        })
    }

    fn begin_install(
        &self,
        app_id: &AppId,
        prepared: &PreparedPackage,
        paths: &InstallPaths,
    ) -> Result<Option<String>, PackageError> {
        let previous_state_text = read_optional_text(&paths.state)?;
        let previous_state = previous_state_text
            .as_deref()
            .map(serde_json::from_str::<InstalledPackageStateV1>)
            .transpose()
            .map_err(|error| PackageError::State(error.to_string()))?;
        let previous_content_id = previous_state
            .as_ref()
            .map(|state| state.current_content_id.clone())
            .or_else(|| current_content_id(&paths.app_root.join("current")));
        let journal = TransactionJournalV1 {
            schema: 1,
            app_id: app_id.to_string(),
            target_content_id: prepared.bundle.content_id.clone(),
            previous_content_id: previous_content_id.clone(),
            previous_manifest: read_optional_text(&paths.manifest)?,
            previous_state: previous_state_text,
        };
        atomic_write(
            &paths.journal,
            &serde_json::to_vec_pretty(&journal)
                .map_err(|error| PackageError::State(error.to_string()))?,
            0o600,
        )?;
        Ok(previous_content_id)
    }

    fn publish_install(
        &self,
        app_id: &AppId,
        prepared: &PreparedPackage,
        paths: &InstallPaths,
        previous_content_id: Option<String>,
    ) -> Result<InstallOutcome, PackageError> {
        let manifest_bytes = materialize_manifest_bytes(
            &prepared.manifest_bytes,
            app_id,
            &prepared.bundle.content_id,
        )?;
        fs::rename(&prepared.stage_root, &paths.release_root)
            .map_err(|source| PackageError::Io(paths.release_root.clone(), source))?;
        make_read_only(&paths.release_root)?;
        atomic_write(&paths.manifest, &manifest_bytes, 0o644)?;
        atomic_symlink(
            Path::new("releases")
                .join(&prepared.bundle.content_id)
                .as_path(),
            &paths.app_root.join("current"),
        )?;
        let state = InstalledPackageStateV1 {
            schema: super::PACKAGE_STATE_SCHEMA_V1,
            app_id: app_id.to_string(),
            package: prepared.bundle.package.clone(),
            current_content_id: prepared.bundle.content_id.clone(),
            previous_content_id: previous_content_id
                .clone()
                .filter(|value| value != &prepared.bundle.content_id),
            version: prepared.bundle.version.clone(),
        };
        atomic_write(
            &paths.state,
            &serde_json::to_vec_pretty(&state)
                .map_err(|error| PackageError::State(error.to_string()))?,
            0o600,
        )?;
        fs::remove_file(&paths.journal)
            .map_err(|source| PackageError::Io(paths.journal.clone(), source))?;
        self.remove_stale_releases(app_id, &state)?;
        Ok(InstallOutcome {
            app_id: app_id.clone(),
            version: prepared.bundle.version.clone(),
            content_id: prepared.bundle.content_id.clone(),
            previous_content_id,
        })
    }
}
