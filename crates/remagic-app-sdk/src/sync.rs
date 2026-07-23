use remagic_core::AppId;
use remagic_protocol::{Request, Response, SyncAction, MAX_FRAME};
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use thiserror::Error;

/// Narrow client for ReMagic's adapter-owned data exchange boundary. This is
/// intentionally separate from the manager UI/control API.
pub struct SyncClient {
    socket: PathBuf,
    requester: AppId,
    provider: AppId,
}

impl SyncClient {
    pub fn new(socket: impl Into<PathBuf>, requester: AppId, provider: AppId) -> Self {
        Self {
            socket: socket.into(),
            requester,
            provider,
        }
    }

    pub fn prepare(&self) -> Result<String, SyncError> {
        self.request(SyncAction::Prepare)
    }

    pub fn export(&self, output: impl Into<PathBuf>) -> Result<String, SyncError> {
        self.request(SyncAction::Export {
            output: output.into(),
        })
    }

    pub fn import(&self, input: impl Into<PathBuf>) -> Result<String, SyncError> {
        self.request(SyncAction::Import {
            input: input.into(),
        })
    }

    pub fn finish(&self) -> Result<String, SyncError> {
        self.request(SyncAction::Finish)
    }

    pub fn exchange_root(data_home: &Path) -> PathBuf {
        data_home.join("sync-exchange")
    }

    fn request(&self, action: SyncAction) -> Result<String, SyncError> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(125)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let request = Request::Sync {
            requester: self.requester.clone(),
            provider: self.provider.clone(),
            action,
        };
        write_frame(&mut stream, &request)?;
        stream.shutdown(Shutdown::Write)?;
        let response: Response = read_frame(&mut stream)?;
        match response {
            Response::SyncOutput {
                success: true,
                output,
            } => Ok(output),
            Response::SyncOutput {
                success: false,
                output,
            }
            | Response::Error { message: output } => Err(SyncError::Rejected(output)),
            _ => Err(SyncError::UnexpectedResponse),
        }
    }
}

fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> Result<(), SyncError> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(SyncError::FrameLength(bytes.len()));
    }
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()?;
    Ok(())
}

fn read_frame<T: for<'de> serde::Deserialize<'de>>(
    stream: &mut UnixStream,
) -> Result<T, SyncError> {
    let mut length = [0_u8; 4];
    stream.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME {
        return Err(SyncError::FrameLength(length));
    }
    let mut bytes = vec![0_u8; length];
    stream.read_exact(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("sync frame length is invalid: {0}")]
    FrameLength(usize),
    #[error("sync request was rejected: {0}")]
    Rejected(String),
    #[error("sync broker returned an unexpected response")]
    UnexpectedResponse,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exchange_root_is_private_to_the_requester_data_home() {
        assert_eq!(
            SyncClient::exchange_root(Path::new("/home/root/.local/share/upload")),
            Path::new("/home/root/.local/share/upload/sync-exchange")
        );
    }
}
