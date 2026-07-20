use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AppId(String);

impl AppId {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !value.starts_with('-')
            && !value.ends_with('-');
        if valid {
            Ok(Self(value))
        } else {
            Err(ManifestError::InvalidId(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for AppId {
    type Error = ManifestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AppId> for String {
    fn from(value: AppId) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParkStrategy {
    /// Persist UI state and exit. Recall starts a fresh UI process.
    #[default]
    Restart,
    /// Keep a process alive after it has acknowledged display/input release.
    Resident,
}

fn default_schema() -> u32 {
    1
}

fn default_display() -> String {
    "quill".into()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppManifest {
    #[serde(default = "default_schema")]
    pub schema: u32,
    pub id: AppId,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub icon: Option<PathBuf>,
    #[serde(default)]
    pub package: Option<String>,
    pub exec: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    #[serde(default = "default_display")]
    pub display: String,
    #[serde(default)]
    pub park_strategy: ParkStrategy,
    #[serde(default)]
    pub background_unit: Option<String>,
    #[serde(default)]
    pub supports_open_path: bool,
    #[serde(default)]
    pub allowed_open_roots: Vec<PathBuf>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

const RESERVED_ENV: &[&str] = &[
    "PATH",
    "REMAGIC_SOCKET",
    "REMAGIC_APP_ID",
    "REMAGIC_LAUNCH_ID",
];

impl AppManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema != 1 {
            return Err(ManifestError::UnsupportedSchema(self.schema));
        }
        if self.name.trim().is_empty() {
            return Err(ManifestError::EmptyName(self.id.to_string()));
        }
        validate_absolute("exec", &self.exec)?;
        validate_absolute("working_dir", &self.working_dir)?;
        if let Some(icon) = &self.icon {
            validate_absolute("icon", icon)?;
        }
        for root in &self.allowed_open_roots {
            validate_absolute("allowed_open_roots", root)?;
        }
        if !matches!(
            self.display.as_str(),
            "qtfb" | "quill" | "einkface" | "none"
        ) {
            return Err(ManifestError::UnsupportedDisplay(self.display.clone()));
        }
        for key in self.environment.keys() {
            if key.is_empty()
                || key.contains('=')
                || key.bytes().any(|b| b == 0)
                || RESERVED_ENV.contains(&key.as_str())
            {
                return Err(ManifestError::UnsafeEnvironment(key.clone()));
            }
        }
        if self
            .args
            .iter()
            .any(|arg| arg.bytes().any(|byte| byte == 0))
        {
            return Err(ManifestError::InvalidArgument);
        }
        Ok(())
    }

    pub fn validate_open_path(&self, path: &Path) -> Result<PathBuf, ManifestError> {
        if !self.supports_open_path {
            return Err(ManifestError::OpenPathUnsupported(self.id.to_string()));
        }
        let canonical = path
            .canonicalize()
            .map_err(|source| ManifestError::Canonicalize(path.to_path_buf(), source))?;
        let allowed = self.allowed_open_roots.iter().any(|root| {
            root.canonicalize()
                .map(|root| canonical == root || canonical.starts_with(&root))
                .unwrap_or(false)
        });
        if allowed {
            Ok(canonical)
        } else {
            Err(ManifestError::OpenPathDenied(canonical))
        }
    }
}

fn validate_absolute(field: &'static str, path: &Path) -> Result<(), ManifestError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(ManifestError::UnsafePath(field, path.to_path_buf()));
    }
    Ok(())
}

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

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid application id: {0}")]
    InvalidId(String),
    #[error("unsupported manifest schema {0}")]
    UnsupportedSchema(u32),
    #[error("application {0} has an empty name")]
    EmptyName(String),
    #[error("unsafe {0} path: {1}")]
    UnsafePath(&'static str, PathBuf),
    #[error("unsupported display backend: {0}")]
    UnsupportedDisplay(String),
    #[error("unsafe environment key: {0}")]
    UnsafeEnvironment(String),
    #[error("application argument contains NUL")]
    InvalidArgument,
    #[error("application {0} does not support open-path")]
    OpenPathUnsupported(String),
    #[error("open path is outside the application's allowed roots: {0}")]
    OpenPathDenied(PathBuf),
    #[error("cannot canonicalize {0}: {1}")]
    Canonicalize(PathBuf, #[source] std::io::Error),
    #[error("cannot list manifest directory {0}: {1}")]
    ReadDir(PathBuf, #[source] std::io::Error),
    #[error("cannot read manifest {0}: {1}")]
    Read(PathBuf, #[source] std::io::Error),
    #[error("cannot parse manifest {0}: {1}")]
    Parse(PathBuf, #[source] toml::de::Error),
    #[error("duplicate application id in {0}")]
    DuplicateId(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_are_strict() {
        assert!(AppId::new("magicpaper").is_ok());
        assert!(AppId::new("ko-reader2").is_ok());
        assert!(AppId::new("KOReader").is_err());
        assert!(AppId::new("../reader").is_err());
        assert!(AppId::new("-reader").is_err());
    }

    #[test]
    fn reserved_environment_is_rejected() {
        let mut env = BTreeMap::new();
        env.insert("REMAGIC_SOCKET".into(), "/tmp/fake".into());
        let app = AppManifest {
            schema: 1,
            id: AppId::new("test").unwrap(),
            name: "Test".into(),
            description: String::new(),
            version: String::new(),
            icon: None,
            package: None,
            exec: "/bin/true".into(),
            args: vec![],
            working_dir: "/tmp".into(),
            display: "none".into(),
            park_strategy: ParkStrategy::Restart,
            background_unit: None,
            supports_open_path: false,
            allowed_open_roots: vec![],
            environment: env,
        };
        assert!(matches!(
            app.validate(),
            Err(ManifestError::UnsafeEnvironment(_))
        ));
    }

    #[test]
    fn qtfb_display_backend_is_accepted() {
        let app = AppManifest {
            schema: 1,
            id: AppId::new("test").unwrap(),
            name: "Test".into(),
            description: String::new(),
            version: String::new(),
            icon: None,
            package: None,
            exec: "/usr/bin/test".into(),
            args: Vec::new(),
            working_dir: "/tmp".into(),
            display: "qtfb".into(),
            park_strategy: ParkStrategy::Restart,
            background_unit: None,
            supports_open_path: false,
            allowed_open_roots: Vec::new(),
            environment: BTreeMap::new(),
        };

        assert!(app.validate().is_ok());
    }
}
