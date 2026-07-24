use remagic_core::{AppId, AppSession, DomainState, PowerSnapshot};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::PathBuf;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

mod agent;
mod control_v2;
mod display;
mod lifecycle;
mod runtime_app;

pub use agent::{
    read_agent_frame, write_agent_frame, AgentClientMessage, AgentErrorCode, AgentEvent,
    AgentFrameError, AgentHistoryTurn, AgentLane, AgentProfile, AgentRuntimeSource, AgentStatus,
    AgentToolDefinition, AGENT_MAX_FRAME, AGENT_PROTOCOL_V1, DEFAULT_AGENT_SOCKET,
};
pub use control_v2::{
    AppViewV2, ControlErrorCode, ControlEvent, ControlEventEnvelope, ControlIntent, ControlReply,
    ControlRequest, ControlResponse, LegacyControlConversionError, SupervisorSnapshot,
};
pub use display::{
    DamageRect, DisplayClientMessage, DisplayErrorCode, DisplayHostMessage, DisplayValidationError,
    FrameCommit, FrameIntent, InkCancel, InkCommit, LeaseRevocationReason, PenFrame, PenPhase,
    PenTool, PixelFormat, SurfaceDescriptor, TouchFrame, TouchPhase,
};
pub use lifecycle::{
    LifecycleCommand, LifecycleCommandBody, LifecycleCommandEnvelope, LifecycleCompatibilityError,
    LifecycleEvent, LifecycleEventBody, LifecycleEventEnvelope, LifecycleStage,
    LifecycleValidationError, ShutdownReason,
};
pub use runtime_app::{
    InputMode, RuntimeAppCommand, RuntimeAppReply, RuntimeAppRequest, RUNTIME_APP_PROTOCOL_V1,
    RUNTIME_APP_PROTOCOL_V2,
};

pub const DEFAULT_SOCKET: &str = "/run/remagic/control.sock";
pub const MAX_FRAME: usize = 64 * 1024;
pub const PROTOCOL_V2: u16 = 2;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Envelope<T> {
    pub protocol: u16,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_state_revision: Option<u64>,
    pub body: T,
}

impl<T> Envelope<T> {
    pub fn new(request_id: impl Into<String>, body: T) -> Self {
        Self {
            protocol: PROTOCOL_V2,
            request_id: request_id.into(),
            expected_state_revision: None,
            body,
        }
    }

    pub fn with_expected_revision(mut self, state_revision: u64) -> Self {
        self.expected_state_revision = Some(state_revision);
        self
    }

    pub fn validate_header(&self) -> Result<(), ProtocolValidationError> {
        if self.protocol != PROTOCOL_V2 {
            return Err(ProtocolValidationError::UnsupportedProtocol(self.protocol));
        }
        let valid = !self.request_id.is_empty()
            && self.request_id.len() <= 128
            && self.request_id.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':')
            });
        if !valid {
            return Err(ProtocolValidationError::InvalidRequestId(
                self.request_id.clone(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ProtocolValidationError {
    #[error("unsupported protocol version {0}")]
    UnsupportedProtocol(u16),
    #[error("invalid request id: {0}")]
    InvalidRequestId(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Status,
    PowerStatus,
    SetIdleSuspend {
        /// Zero disables automatic suspension; otherwise the platform
        /// validates the system-supported range.
        seconds: u64,
    },
    ListApps,
    ReloadManifests,
    OpenManager,
    ReturnSystem,
    Sleep {
        /// QTFB commit sequence containing the fully rendered lock page.
        /// Zero is invalid; suspend never proceeds on unproven pixels.
        lock_surface_sequence: u64,
    },
    Wake {
        /// QTFB commit sequence containing the manager page that must replace
        /// the lock image before input is released.
        manager_surface_sequence: u64,
    },
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
    RuntimeExited {
        app_id: AppId,
        generation: u64,
        exit_code: i32,
        #[serde(default)]
        crashed: bool,
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
    Sync {
        requester: AppId,
        provider: AppId,
        action: SyncAction,
    },
    Package {
        operation: PackageOperation,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SyncAction {
    Prepare,
    Export { output: PathBuf },
    Import { input: PathBuf },
    Finish,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    Power {
        snapshot: PowerSnapshot,
    },
    Apps {
        apps: Vec<AppView>,
    },
    PackageOutput {
        success: bool,
        output: String,
    },
    SyncOutput {
        success: bool,
        output: String,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AppCommand {
    EnterForeground {
        #[serde(default)]
        resume_payload: Option<serde_json::Value>,
        #[serde(default)]
        open_path: Option<PathBuf>,
        /// Optional display-host fence for schema-v2 callers. Transitional
        /// callers may omit both fields and let the runner advance its local
        /// compatibility token.
        #[serde(default)]
        foreground_epoch: Option<u64>,
        #[serde(default)]
        lease_id: Option<u64>,
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

    #[test]
    fn v2_header_boundaries_are_validated() {
        for id in ["a".to_owned(), "a".repeat(128), "req:1_test.v2".to_owned()] {
            assert!(Envelope::new(id, ControlIntent::Snapshot)
                .validate_header()
                .is_ok());
        }
        for id in [String::new(), "a".repeat(129), "contains space".into()] {
            assert!(matches!(
                Envelope::new(id, ControlIntent::Snapshot).validate_header(),
                Err(ProtocolValidationError::InvalidRequestId(_))
            ));
        }
        let mut envelope = Envelope::new("request", ControlIntent::Snapshot);
        envelope.protocol = 1;
        assert_eq!(
            envelope.validate_header(),
            Err(ProtocolValidationError::UnsupportedProtocol(1))
        );
    }

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

    #[test]
    fn sleep_and_wake_requests_carry_their_proven_surface_sequences() {
        for request in [
            Request::Sleep {
                lock_surface_sequence: 27,
            },
            Request::Wake {
                manager_surface_sequence: 29,
            },
        ] {
            let encoded = serde_json::to_vec(&request).unwrap();
            assert_eq!(
                serde_json::from_slice::<Request>(&encoded).unwrap(),
                request
            );
        }
    }

    #[test]
    fn power_policy_requests_round_trip_without_legacy_ambiguity() {
        for request in [
            Request::PowerStatus,
            Request::SetIdleSuspend { seconds: 120 },
        ] {
            let encoded = serde_json::to_vec(&request).unwrap();
            assert_eq!(
                serde_json::from_slice::<Request>(&encoded).unwrap(),
                request
            );
        }
    }

    #[tokio::test]
    async fn invalid_frame_lengths_are_rejected_before_allocation() {
        for length in [0_u32, (MAX_FRAME + 1) as u32] {
            let (mut left, mut right) = tokio::io::duplex(16);
            let send = tokio::spawn(async move {
                left.write_u32(length).await.unwrap();
            });
            let error = read_frame::<_, Request>(&mut right).await.unwrap_err();
            send.await.unwrap();
            assert!(matches!(error, FrameError::Length(actual) if actual == length as usize));
        }
    }

    #[tokio::test]
    async fn oversized_serialized_frame_is_not_written() {
        let (mut writer, _reader) = tokio::io::duplex(16);
        let response = Response::Error {
            message: "x".repeat(MAX_FRAME),
        };
        let error = write_frame(&mut writer, &response).await.unwrap_err();
        assert!(matches!(error, FrameError::Length(actual) if actual > MAX_FRAME));
    }

    #[test]
    fn runtime_exit_round_trip_preserves_generation_and_status() {
        let request = Request::RuntimeExited {
            app_id: AppId::new("koreader").unwrap(),
            generation: 4_271_337,
            exit_code: 9,
            crashed: true,
        };
        let bytes = serde_json::to_vec(&request).unwrap();
        let decoded: Request = serde_json::from_slice(&bytes).unwrap();
        assert!(matches!(
            decoded,
            Request::RuntimeExited {
                app_id,
                generation: 4_271_337,
                exit_code: 9,
                crashed: true,
            } if app_id.as_str() == "koreader"
        ));
    }

    #[test]
    fn foreground_command_preserves_v2_fence_and_accepts_legacy_omission() {
        let command = AppCommand::EnterForeground {
            resume_payload: Some(serde_json::json!({"page": 9})),
            open_path: None,
            foreground_epoch: Some(41),
            lease_id: Some(73),
        };
        let value = serde_json::to_value(&command).unwrap();
        assert_eq!(value["type"], "enter_foreground");
        assert_eq!(value["foreground_epoch"], 41);
        assert_eq!(value["lease_id"], 73);
        assert_eq!(
            serde_json::from_value::<AppCommand>(value).unwrap(),
            command
        );

        let legacy: AppCommand = serde_json::from_str(r#"{"type":"enter_foreground"}"#).unwrap();
        assert!(matches!(
            legacy,
            AppCommand::EnterForeground {
                foreground_epoch: None,
                lease_id: None,
                ..
            }
        ));
    }
}
