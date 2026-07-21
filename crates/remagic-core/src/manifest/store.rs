use super::{AppId, AppManifest, ManifestError};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct ManifestStore {
    root: PathBuf,
}

impl ManifestStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn load_all(&self) -> Result<BTreeMap<AppId, AppManifest>, ManifestError> {
        let mut manifests = BTreeMap::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(manifests),
            Err(source) => return Err(ManifestError::ReadDir(self.root.clone(), source)),
        };
        for entry in entries {
            let entry =
                entry.map_err(|source| ManifestError::ReadDir(self.root.clone(), source))?;
            let path = entry.path();
            if path.extension().and_then(|v| v.to_str()) != Some("toml") {
                continue;
            }
            let text = fs::read_to_string(&path)
                .map_err(|source| ManifestError::Read(path.clone(), source))?;
            let manifest: AppManifest = toml::from_str(&text)
                .map_err(|source| ManifestError::Parse(path.clone(), source))?;
            manifest.validate()?;
            if manifests.insert(manifest.id.clone(), manifest).is_some() {
                return Err(ManifestError::DuplicateId(path));
            }
        }
        Ok(manifests)
    }
}
