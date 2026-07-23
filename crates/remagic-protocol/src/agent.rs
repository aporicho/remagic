//! Application-to-platform Pi agent protocol.
//!
//! The channel is deliberately separate from manager control. Every message is
//! a length-prefixed JSON object with identity at the top level, making traces
//! and independent clients straightforward without granting system access.

use remagic_core::{AppId, DeviceProfile};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const AGENT_PROTOCOL_V1: u16 = 1;
pub const DEFAULT_AGENT_SOCKET: &str = "/run/remagic/agent.sock";
pub const AGENT_MAX_FRAME: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentLane {
    #[default]
    Interactive,
    Speculative,
    Scheduled,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentProfile {
    pub provider: String,
    pub model: String,
    #[serde(default = "default_thinking")]
    pub thinking: String,
    #[serde(default)]
    pub tools: bool,
}

fn default_thinking() -> String {
    "off".into()
}

impl AgentProfile {
    pub fn validate(&self) -> Result<(), AgentValidationError> {
        validate_name("provider", &self.provider, 64)?;
        validate_name("model", &self.model, 128)?;
        if !matches!(
            self.thinking.as_str(),
            "off" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        ) {
            return Err(AgentValidationError::InvalidField("thinking"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentHistoryTurn {
    pub user: String,
    pub assistant: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AgentToolDefinition {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parameters: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentClientMessage {
    Status {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        client_token: String,
    },
    StartTurn {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        client_token: String,
        profile: AgentProfile,
        #[serde(default)]
        lane: AgentLane,
        #[serde(default)]
        system_prompt: String,
        input: String,
        #[serde(default)]
        history: Vec<AgentHistoryTurn>,
        #[serde(default)]
        tools: Vec<AgentToolDefinition>,
    },
    CancelTurn {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        client_token: String,
        turn_id: String,
    },
    ToolResult {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        client_token: String,
        turn_id: String,
        tool_call_id: String,
        result: Value,
        #[serde(default)]
        is_error: bool,
    },
    ReloadProfile {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        client_token: String,
        #[serde(default)]
        profile: Option<AgentProfile>,
    },
    NewSession {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        client_token: String,
    },
}

impl AgentClientMessage {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Status { request_id, .. }
            | Self::StartTurn { request_id, .. }
            | Self::CancelTurn { request_id, .. }
            | Self::ToolResult { request_id, .. }
            | Self::ReloadProfile { request_id, .. }
            | Self::NewSession { request_id, .. } => request_id,
        }
    }

    pub fn app_id(&self) -> &AppId {
        match self {
            Self::Status { app_id, .. }
            | Self::StartTurn { app_id, .. }
            | Self::CancelTurn { app_id, .. }
            | Self::ToolResult { app_id, .. }
            | Self::ReloadProfile { app_id, .. }
            | Self::NewSession { app_id, .. } => app_id,
        }
    }

    pub fn client_token(&self) -> &str {
        match self {
            Self::Status { client_token, .. }
            | Self::StartTurn { client_token, .. }
            | Self::CancelTurn { client_token, .. }
            | Self::ToolResult { client_token, .. }
            | Self::ReloadProfile { client_token, .. }
            | Self::NewSession { client_token, .. } => client_token,
        }
    }

    pub fn validate(&self) -> Result<(), AgentValidationError> {
        let protocol = match self {
            Self::Status { protocol, .. }
            | Self::StartTurn { protocol, .. }
            | Self::CancelTurn { protocol, .. }
            | Self::ToolResult { protocol, .. }
            | Self::ReloadProfile { protocol, .. }
            | Self::NewSession { protocol, .. } => *protocol,
        };
        if protocol != AGENT_PROTOCOL_V1 {
            return Err(AgentValidationError::UnsupportedProtocol(protocol));
        }
        validate_name("request_id", self.request_id(), 128)?;
        validate_token(self.client_token())?;
        match self {
            Self::StartTurn {
                profile,
                system_prompt,
                input,
                history,
                tools,
                ..
            } => {
                profile.validate()?;
                if input.trim().is_empty() || input.len() > 256 * 1024 {
                    return Err(AgentValidationError::InvalidField("input"));
                }
                if system_prompt.len() > 64 * 1024 || history.len() > 64 || tools.len() > 32 {
                    return Err(AgentValidationError::RequestTooLarge);
                }
                for tool in tools {
                    validate_name("tool.name", &tool.name, 96)?;
                }
            }
            Self::CancelTurn { turn_id, .. } | Self::ToolResult { turn_id, .. } => {
                validate_name("turn_id", turn_id, 128)?;
            }
            Self::ReloadProfile { profile, .. } => {
                if let Some(profile) = profile {
                    profile.validate()?;
                }
            }
            Self::Status { .. } | Self::NewSession { .. } => {}
        }
        if let Self::ToolResult { tool_call_id, .. } = self {
            validate_name("tool_call_id", tool_call_id, 128)?;
        }
        Ok(())
    }
}

fn validate_token(value: &str) -> Result<(), AgentValidationError> {
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some(())
        .ok_or(AgentValidationError::InvalidField("client_token"))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub available: bool,
    pub provider_configured: bool,
    pub runtime_source: AgentRuntimeSource,
    pub busy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<AgentProfile>,
    pub device: DeviceProfile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRuntimeSource {
    Packaged,
    Legacy,
    Override,
    Missing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentErrorCode {
    InvalidRequest,
    Unavailable,
    Busy,
    TurnNotFound,
    ToolNotPending,
    BackendFailed,
    Cancelled,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Accepted {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        turn_id: String,
    },
    TextDelta {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        turn_id: String,
        text: String,
    },
    ToolCall {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        turn_id: String,
        tool_call_id: String,
        name: String,
        arguments: Value,
    },
    Complete {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        turn_id: String,
    },
    Error {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        code: AgentErrorCode,
        message: String,
        #[serde(default)]
        retryable: bool,
    },
    Status {
        protocol: u16,
        request_id: String,
        app_id: AppId,
        status: Box<AgentStatus>,
    },
}

fn validate_name(
    field: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), AgentValidationError> {
    let valid = !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        });
    valid
        .then_some(())
        .ok_or(AgentValidationError::InvalidField(field))
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AgentValidationError {
    #[error("unsupported agent protocol {0}")]
    UnsupportedProtocol(u16),
    #[error("invalid agent field {0}")]
    InvalidField(&'static str),
    #[error("agent request exceeds structural limits")]
    RequestTooLarge,
}

pub async fn read_agent_frame<R>(reader: &mut R) -> Result<AgentClientMessage, AgentFrameError>
where
    R: AsyncRead + Unpin,
{
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > AGENT_MAX_FRAME {
        return Err(AgentFrameError::Length(length));
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn write_agent_frame<W>(writer: &mut W, value: &AgentEvent) -> Result<(), AgentFrameError>
where
    W: AsyncWrite + Unpin,
{
    let bytes = serde_json::to_vec(value)?;
    if bytes.is_empty() || bytes.len() > AGENT_MAX_FRAME {
        return Err(AgentFrameError::Length(bytes.len()));
    }
    writer.write_u32(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[derive(Debug, Error)]
pub enum AgentFrameError {
    #[error("agent I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid agent frame length {0}")]
    Length(usize),
    #[error("invalid agent JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AppId {
        AppId::new("magicpaper").unwrap()
    }

    #[test]
    fn planned_start_turn_wire_shape_is_stable() {
        let json = r#"{"protocol":1,"type":"start_turn","request_id":"r1","app_id":"magicpaper","client_token":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","profile":{"provider":"deepseek","model":"deepseek-v4-flash","thinking":"off","tools":true},"system_prompt":"paper","input":"hello","history":[{"user":"u","assistant":"a"}],"tools":[]}"#;
        let message: AgentClientMessage = serde_json::from_str(json).unwrap();
        assert_eq!(message.app_id(), &app());
        assert!(message.validate().is_ok());
        assert_eq!(
            serde_json::to_value(&message).unwrap()["type"],
            "start_turn"
        );
    }

    #[tokio::test]
    async fn agent_frames_are_big_endian_and_round_trip() {
        let event = AgentEvent::TextDelta {
            protocol: 1,
            request_id: "r1".into(),
            app_id: app(),
            turn_id: "t1".into(),
            text: "答案".into(),
        };
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        let expected = event.clone();
        let task = tokio::spawn(async move { write_agent_frame(&mut writer, &event).await });
        let length = reader.read_u32().await.unwrap() as usize;
        let mut body = vec![0; length];
        reader.read_exact(&mut body).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<AgentEvent>(&body).unwrap(),
            expected
        );
        task.await.unwrap().unwrap();
    }

    #[test]
    fn unsafe_or_unbounded_requests_fail_validation() {
        let invalid = AgentClientMessage::StartTurn {
            protocol: 1,
            request_id: "contains space".into(),
            app_id: app(),
            client_token: "bad".into(),
            profile: AgentProfile {
                provider: "deepseek;sh".into(),
                model: "m".into(),
                thinking: "off".into(),
                tools: false,
            },
            lane: AgentLane::Interactive,
            system_prompt: String::new(),
            input: "hello".into(),
            history: Vec::new(),
            tools: Vec::new(),
        };
        assert!(invalid.validate().is_err());
    }
}
