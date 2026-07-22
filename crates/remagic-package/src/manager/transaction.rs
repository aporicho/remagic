use super::{remove_optional_file, PackageError, PackageManager, TransactionJournalV1};
use crate::filesystem::{atomic_symlink, atomic_write, remove_tree};
use remagic_core::AppId;
use std::fs;
use std::path::Path;

impl PackageManager {
    pub fn recover_all(&self) -> Result<(), PackageError> {
        let entries = match fs::read_dir(&self.paths.state_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(PackageError::Io(self.paths.state_root.clone(), source)),
        };
        for entry in entries {
            let entry =
                entry.map_err(|source| PackageError::Io(self.paths.state_root.clone(), source))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) == Some("journal") {
                self.recover_journal(&path)?;
            }
        }
        Ok(())
    }

    pub(super) fn recover_for(&self, app_id: &AppId) -> Result<(), PackageError> {
        let path = self.journal_path(app_id);
        if path.exists() {
            self.recover_journal(&path)?;
        }
        Ok(())
    }

    pub(super) fn recover_journal(&self, path: &Path) -> Result<(), PackageError> {
        let bytes = fs::read(path).map_err(|source| PackageError::Io(path.into(), source))?;
        let journal: TransactionJournalV1 = serde_json::from_slice(&bytes)
            .map_err(|error| PackageError::State(error.to_string()))?;
        if journal.schema != 1 {
            return Err(PackageError::State(
                "unsupported transaction journal".into(),
            ));
        }
        let app_id = AppId::new(journal.app_id.clone())
            .map_err(|error| PackageError::State(error.to_string()))?;
        let app_root = self.paths.apps_root.join(app_id.as_str());
        let current = app_root.join("current");
        match journal.previous_content_id {
            Some(previous) => {
                atomic_symlink(Path::new("releases").join(previous).as_path(), &current)?;
            }
            None => remove_optional_file(&current)?,
        }
        match journal.previous_manifest {
            Some(text) => atomic_write(&self.manifest_path(&app_id), text.as_bytes(), 0o644)?,
            None => remove_optional_file(&self.manifest_path(&app_id))?,
        }
        match journal.previous_state {
            Some(text) => atomic_write(&self.state_path(&app_id), text.as_bytes(), 0o600)?,
            None => remove_optional_file(&self.state_path(&app_id))?,
        }
        remove_tree(&app_root.join("releases").join(journal.target_content_id))?;
        remove_optional_file(path)
    }
}
