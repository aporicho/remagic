//! Canonical application lifecycle messages and their narrow v1 compatibility
//! conversions. Commands, events, and validation form one wire-schema boundary.

use crate::Envelope;
use remagic_core::{AppToken, LaunchEnvironment};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

pub type LifecycleCommandEnvelope = Envelope<LifecycleCommandBody>;
pub type LifecycleEventEnvelope = Envelope<LifecycleEventBody>;

/// `token` is kept at the top of the envelope body so small adapter bridges
/// can reject stale messages before decoding command-specific payloads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LifecycleCommandBody {
    pub token: AppToken,
    #[serde(flatten)]
    pub command: LifecycleCommand,
}

impl LifecycleCommandBody {
    pub fn validate(&self) -> Result<(), LifecycleValidationError> {
        self.token
            .validate()
            .map_err(|error| LifecycleValidationError::Token(error.to_string()))?;
        match &self.command {
            LifecycleCommand::Start {
                launch_environment, ..
            } => {
                if launch_environment.app_id != self.token.app_id {
                    return Err(LifecycleValidationError::EnvironmentAppMismatch);
                }
                launch_environment
                    .validate()
                    .map_err(|error| LifecycleValidationError::Environment(error.to_string()))?;
            }
            LifecycleCommand::Shutdown { deadline_ms, .. } if *deadline_ms == 0 => {
                return Err(LifecycleValidationError::ZeroShutdownDeadline)
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum LifecycleCommand {
    Start {
        launch_environment: Box<LaunchEnvironment>,
        #[serde(default)]
        resume_payload: Option<serde_json::Value>,
        #[serde(default)]
        open_path: Option<PathBuf>,
    },
    EnterForeground {
        #[serde(default)]
        resume_payload: Option<serde_json::Value>,
        #[serde(default)]
        open_path: Option<PathBuf>,
    },
    EnterBackground,
    OpenPath {
        path: PathBuf,
    },
    Shutdown {
        reason: ShutdownReason,
        deadline_ms: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    User,
    Upgrade,
    ReturnStock,
    Recovery,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LifecycleEventBody {
    pub token: AppToken,
    #[serde(flatten)]
    pub event: LifecycleEvent,
}

impl LifecycleEventBody {
    pub fn validate(&self) -> Result<(), LifecycleValidationError> {
        self.token
            .validate()
            .map_err(|error| LifecycleValidationError::Token(error.to_string()))?;
        if matches!(
            self.event,
            LifecycleEvent::Ready {
                first_frame_sequence: Some(0)
            }
        ) {
            return Err(LifecycleValidationError::ZeroFirstFrameSequence);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum LifecycleEvent {
    Ready {
        #[serde(default)]
        first_frame_sequence: Option<u64>,
    },
    BackgroundReady {
        #[serde(default)]
        title: String,
        #[serde(default)]
        subtitle: String,
        #[serde(default)]
        resume_payload: Option<serde_json::Value>,
    },
    StateSaved {
        #[serde(default)]
        resume_payload: Option<serde_json::Value>,
    },
    ShutdownComplete {
        exit_code: i32,
    },
    Failed {
        stage: LifecycleStage,
        message: String,
        #[serde(default)]
        retryable: bool,
    },
    /// Transitional v1 notification callback.
    Notification {
        title: String,
        body: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStage {
    Start,
    Foreground,
    Background,
    Save,
    Shutdown,
    Runtime,
}

impl TryFrom<&LifecycleCommandBody> for crate::AppCommand {
    type Error = LifecycleCompatibilityError;

    fn try_from(body: &LifecycleCommandBody) -> Result<Self, Self::Error> {
        Ok(match &body.command {
            LifecycleCommand::Start { .. } => crate::AppCommand::Resume,
            LifecycleCommand::EnterForeground {
                resume_payload,
                open_path,
            } => crate::AppCommand::EnterForeground {
                resume_payload: resume_payload.clone(),
                open_path: open_path.clone(),
                foreground_epoch: Some(body.token.foreground_epoch),
                lease_id: body.token.lease_id,
            },
            LifecycleCommand::EnterBackground => crate::AppCommand::PreparePark,
            LifecycleCommand::OpenPath { path } => crate::AppCommand::EnterForeground {
                resume_payload: None,
                open_path: Some(path.clone()),
                foreground_epoch: Some(body.token.foreground_epoch),
                lease_id: body.token.lease_id,
            },
            LifecycleCommand::Shutdown { .. } => crate::AppCommand::Shutdown,
        })
    }
}

impl LifecycleEventBody {
    pub fn from_v1_request(
        request: crate::Request,
        token: AppToken,
    ) -> Result<Self, LifecycleCompatibilityError> {
        use crate::Request;
        let expected = token.app_id.clone();
        let event = match request {
            Request::Ready { app_id } if app_id == expected => LifecycleEvent::Ready {
                first_frame_sequence: None,
            },
            Request::Parked {
                app_id,
                title,
                subtitle,
                resume_payload,
            } if app_id == expected => LifecycleEvent::BackgroundReady {
                title,
                subtitle,
                resume_payload,
            },
            Request::RuntimeExited {
                app_id,
                generation,
                exit_code,
                crashed,
            } if app_id == expected && generation == token.generation => {
                if crashed {
                    LifecycleEvent::Failed {
                        stage: LifecycleStage::Runtime,
                        message: format!("process exited with status {exit_code}"),
                        retryable: true,
                    }
                } else {
                    LifecycleEvent::ShutdownComplete { exit_code }
                }
            }
            Request::RuntimeExited {
                app_id, generation, ..
            } if app_id == expected => {
                return Err(LifecycleCompatibilityError::TokenGenerationMismatch {
                    expected: token.generation,
                    actual: generation,
                })
            }
            Request::Notify {
                app_id,
                title,
                body,
            } if app_id == expected => LifecycleEvent::Notification { title, body },
            Request::Ready { app_id }
            | Request::Parked { app_id, .. }
            | Request::RuntimeExited { app_id, .. }
            | Request::Notify { app_id, .. } => {
                return Err(LifecycleCompatibilityError::TokenAppMismatch {
                    expected,
                    actual: app_id,
                })
            }
            _ => return Err(LifecycleCompatibilityError::NotLifecycleRequest),
        };
        Ok(Self { token, event })
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LifecycleCompatibilityError {
    #[error("request is not a v1 application lifecycle callback")]
    NotLifecycleRequest,
    #[error("lifecycle token app {expected} does not match request app {actual}")]
    TokenAppMismatch {
        expected: remagic_core::AppId,
        actual: remagic_core::AppId,
    },
    #[error("lifecycle token generation {expected} does not match request generation {actual}")]
    TokenGenerationMismatch { expected: u64, actual: u64 },
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LifecycleValidationError {
    #[error("invalid application token: {0}")]
    Token(String),
    #[error("launch environment belongs to a different application")]
    EnvironmentAppMismatch,
    #[error("invalid launch environment: {0}")]
    Environment(String),
    #[error("shutdown deadline must be greater than zero")]
    ZeroShutdownDeadline,
    #[error("first frame sequence must be greater than zero")]
    ZeroFirstFrameSequence,
}

#[cfg(test)]
mod tests;
