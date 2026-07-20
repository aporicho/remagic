use remagic_core::{AppId, AppSession, DomainState};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const DEFAULT_SOCKET: &str = "/run/remagic/control.sock";
pub const MAX_FRAME: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Status,
    ListApps,
    ReloadManifests,
    OpenManager,
    ReturnSystem,
    Sleep,
    Launch {
        app_id: AppId,
        #[serde(default)]
        open_path: Option<PathBuf>,
    },
    ParkCurrent,
    Close {
        app_id: AppId,
        #[serde(default)]
        complete: bool,
    },
    Ready {
        app_id: AppId,
    },
    Parked {
        app_id: AppId,
        title: String,
        #[serde(default)]
        subtitle: String,
        #[serde(default)]
        resume_payload: Option<serde_json::Value>,
    },
    Notify {
        app_id: AppId,
        title: String,
        body: String,
    },
    Package {
        operation: PackageOperation,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum PackageOperation {
    Bootstrap,
    Refresh,
    Search { query: String },
    Info { package: String },
    Install { package: String },
    Remove { package: String, purge: bool },
    Upgrade,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppView {
    pub id: AppId,
    pub name: String,
    pub description: String,
    pub installed: bool,
    pub foreground: bool,
    pub background_service: Option<String>,
    pub background_active: bool,
    pub session: Option<AppSession>,
    pub package: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Error {
        message: String,
    },
    Status {
        domain: DomainState,
        last_app: Option<AppId>,
        sequence: u64,
    },
    Apps {
        apps: Vec<AppView>,
    },
    PackageOutput {
        success: bool,
        output: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppCommand {
    EnterForeground {
        #[serde(default)]
        resume_payload: Option<serde_json::Value>,
        #[serde(default)]
        open_path: Option<PathBuf>,
    },
    PreparePark,
    EnterBackground,
    Shutdown,
    Resume,
}

pub async fn read_frame<R, T>(reader: &mut R) -> Result<T, FrameError>
where
    R: AsyncRead + Unpin,
    T: for<'de> Deserialize<'de>,
{
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(FrameError::Length(length));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes).map_err(FrameError::Json)
}

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let bytes = serde_json::to_vec(value)?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(FrameError::Length(bytes.len()));
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid frame length {0}")]
    Length(usize),
    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_round_trip() {
        let (mut left, mut right) = tokio::io::duplex(1024);
        let send = tokio::spawn(async move {
            write_frame(&mut left, &Request::Status).await.unwrap();
        });
        let request: Request = read_frame(&mut right).await.unwrap();
        send.await.unwrap();
        assert!(matches!(request, Request::Status));
    }
}
