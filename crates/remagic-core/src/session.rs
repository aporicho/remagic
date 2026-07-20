use crate::AppId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Background,
    Parked,
    Crashed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppSession {
    pub schema: u32,
    pub app_id: AppId,
    pub status: SessionStatus,
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub resume_payload: Option<serde_json::Value>,
    pub updated_at: i64,
    #[serde(default)]
    pub last_error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SessionStore {
    root: PathBuf,
}

impl SessionStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn load_all(&self) -> Result<BTreeMap<AppId, AppSession>, SessionError> {
        let mut sessions = BTreeMap::new();
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(sessions),
            Err(source) => return Err(SessionError::Io(self.root.clone(), source)),
        };
        for entry in entries {
            let entry = entry.map_err(|source| SessionError::Io(self.root.clone(), source))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path).map_err(|source| SessionError::Io(path.clone(), source))?;
            let session: AppSession = serde_json::from_slice(&bytes)
                .map_err(|source| SessionError::Parse(path.clone(), source))?;
            if session.schema != 1 {
                return Err(SessionError::Schema(path, session.schema));
            }
            sessions.insert(session.app_id.clone(), session);
        }
        Ok(sessions)
    }

    pub fn save(&self, session: &AppSession) -> Result<(), SessionError> {
        fs::create_dir_all(&self.root)
            .map_err(|source| SessionError::Io(self.root.clone(), source))?;
        let path = self.path_for(&session.app_id);
        let temp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        let bytes = serde_json::to_vec_pretty(session).map_err(SessionError::Serialize)?;
        let mut file =
            File::create(&temp).map_err(|source| SessionError::Io(temp.clone(), source))?;
        file.write_all(&bytes)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|source| SessionError::Io(temp.clone(), source))?;
        fs::rename(&temp, &path).map_err(|source| SessionError::Io(path.clone(), source))?;
        sync_directory(&self.root).map_err(|source| SessionError::Io(self.root.clone(), source))?;
        Ok(())
    }

    pub fn remove(&self, id: &AppId) -> Result<(), SessionError> {
        let path = self.path_for(id);
        match fs::remove_file(&path) {
            Ok(()) => sync_directory(&self.root)
                .map_err(|source| SessionError::Io(self.root.clone(), source)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(SessionError::Io(path, source)),
        }
    }

    fn path_for(&self, id: &AppId) -> PathBuf {
        self.root.join(format!("{}.json", id.as_str()))
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session I/O error at {0}: {1}")]
    Io(PathBuf, #[source] io::Error),
    #[error("invalid session at {0}: {1}")]
    Parse(PathBuf, #[source] serde_json::Error),
    #[error("unsupported session schema {1} at {0}")]
    Schema(PathBuf, u32),
    #[error("cannot encode session: {0}")]
    Serialize(#[source] serde_json::Error),
}
