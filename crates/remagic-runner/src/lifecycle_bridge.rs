//! Lifecycle orchestration between the runner and an application process.
//! Wire transport, compatibility decoding, token fencing, status publication,
//! and daemon control sockets are isolated behind this facade.

use remagic_core::{AppId, LaunchEnvironment};
use remagic_protocol::{
    AppCommand, Envelope, LifecycleCommand, LifecycleCommandBody, LifecycleEventEnvelope,
    ShutdownReason,
};
use serde_json::Value;
use std::io;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::unix::AsyncFd;
use tokio::sync::Mutex;

mod compatibility;
mod control;
mod status;
mod token;
mod transport;

use compatibility::decode_event;
#[cfg(test)]
use control::serve_control_client;
pub(crate) use control::ControlSocket;
pub(crate) use status::LifecycleStatusStore;
#[cfg(test)]
use token::TokenCursor;
use token::{event_matches_token, TokenState};
#[cfg(test)]
use transport::{decode_packet, encode_command, receive_packet};
use transport::{send_packet, ChildTransport};

#[derive(Clone)]
pub(crate) struct LifecycleBridge {
    descriptor: Arc<AsyncFd<OwnedFd>>,
    transport: ChildTransport,
    token: Arc<Mutex<TokenState>>,
    operation: Arc<Mutex<()>>,
    next_request: Arc<AtomicU64>,
    shutdown_deadline_ms: u64,
}

impl LifecycleBridge {
    pub(crate) fn new(
        descriptor: OwnedFd,
        app_id: AppId,
        generation: u64,
        foreground_epoch: u64,
        lease_id: u64,
        shutdown_deadline_ms: u64,
    ) -> io::Result<Self> {
        Ok(Self {
            descriptor: Arc::new(AsyncFd::new(descriptor)?),
            transport: ChildTransport::for_app(&app_id),
            token: Arc::new(Mutex::new(TokenState::new(
                app_id,
                generation,
                foreground_epoch,
                lease_id,
            ))),
            operation: Arc::new(Mutex::new(())),
            next_request: Arc::new(AtomicU64::new(1)),
            shutdown_deadline_ms,
        })
    }

    pub(crate) async fn send_start(
        &self,
        launch_environment: LaunchEnvironment,
        resume_payload: Option<Value>,
        open_path: Option<PathBuf>,
    ) -> Result<(), BridgeError> {
        let _operation = self.operation.lock().await;
        let token = self.token.lock().await.current().clone();
        self.send_command_locked(LifecycleCommandBody {
            token,
            command: LifecycleCommand::Start {
                launch_environment: Box::new(launch_environment),
                resume_payload,
                open_path,
            },
        })
        .await
    }

    pub(crate) async fn dispatch(&self, command: AppCommand) -> Result<(), BridgeError> {
        let _operation = self.operation.lock().await;
        let mut token = self.token.lock().await;
        let body = command_body(&mut token, command, self.shutdown_deadline_ms)?;
        drop(token);
        self.send_command_locked(body).await
    }

    pub(crate) async fn request_shutdown(&self, reason: ShutdownReason) -> Result<(), BridgeError> {
        let _operation = self.operation.lock().await;
        let token = self.token.lock().await.current().clone();
        self.send_command_locked(LifecycleCommandBody {
            token,
            command: LifecycleCommand::Shutdown {
                reason,
                deadline_ms: self.shutdown_deadline_ms,
            },
        })
        .await
    }

    pub(crate) async fn receive_events(&self) -> Result<Vec<LifecycleEventEnvelope>, BridgeError> {
        let packet = transport::receive_packet(&self.descriptor).await?;
        if packet.is_empty() {
            return Err(BridgeError::Disconnected);
        }
        transport::decode_packet(self.transport, &packet)?
            .into_iter()
            .map(|payload| decode_event(&payload))
            .collect()
    }

    pub(crate) async fn persist_current_event(
        &self,
        status_store: &LifecycleStatusStore,
        envelope: &LifecycleEventEnvelope,
    ) -> Result<bool, BridgeError> {
        // Keep the current-token check and atomic publication in the same
        // operation critical section as foreground token advancement.
        let _operation = self.operation.lock().await;
        let token = self.token.lock().await;
        if !event_matches_token(&envelope.body, token.current()) {
            return Ok(false);
        }
        status_store.write(envelope)?;
        Ok(true)
    }

    async fn send_command_locked(&self, body: LifecycleCommandBody) -> Result<(), BridgeError> {
        body.validate()?;
        let sequence = self.next_request.fetch_add(1, Ordering::Relaxed);
        let request_id = format!(
            "runner-{}-{}-{sequence}",
            body.token.app_id, body.token.generation
        );
        let envelope = Envelope::new(request_id, body);
        envelope.validate_header()?;
        let packet = transport::encode_command(self.transport, &envelope)?;
        send_packet(&self.descriptor, &packet).await?;
        Ok(())
    }
}

fn command_body(
    token: &mut TokenState,
    command: AppCommand,
    shutdown_deadline_ms: u64,
) -> Result<LifecycleCommandBody, BridgeError> {
    let body = match command {
        AppCommand::EnterForeground {
            resume_payload,
            open_path,
            foreground_epoch,
            lease_id,
        } => LifecycleCommandBody {
            token: token.foreground_with_fence(foreground_epoch, lease_id)?,
            command: LifecycleCommand::EnterForeground {
                resume_payload,
                open_path,
            },
        },
        AppCommand::Resume => LifecycleCommandBody {
            token: token.foreground()?,
            command: LifecycleCommand::EnterForeground {
                resume_payload: None,
                open_path: None,
            },
        },
        AppCommand::PreparePark | AppCommand::EnterBackground => LifecycleCommandBody {
            token: token.current().clone(),
            command: LifecycleCommand::EnterBackground,
        },
        AppCommand::Shutdown => LifecycleCommandBody {
            token: token.current().clone(),
            command: LifecycleCommand::Shutdown {
                reason: ShutdownReason::User,
                deadline_ms: shutdown_deadline_ms,
            },
        },
    };
    Ok(body)
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum BridgeError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Frame(#[from] remagic_protocol::FrameError),
    #[error(transparent)]
    Protocol(#[from] remagic_protocol::ProtocolValidationError),
    #[error(transparent)]
    Lifecycle(#[from] remagic_protocol::LifecycleValidationError),
    #[error("lifecycle channel disconnected")]
    Disconnected,
    #[error("invalid lifecycle frame length {0}")]
    FrameLength(usize),
    #[error("invalid lifecycle envelope: {0}")]
    InvalidEnvelope(&'static str),
    #[error("lifecycle envelope is missing {0}")]
    MissingField(&'static str),
    #[error("lifecycle envelope has an invalid {0}")]
    InvalidField(&'static str),
    #[error("unknown lifecycle event {0}")]
    UnknownEvent(String),
    #[error("foreground token epoch space is exhausted")]
    TokenExhausted,
    #[error("foreground_epoch and lease_id must either both be present or both be absent")]
    IncompleteForegroundFence,
    #[error("foreground_epoch and lease_id must both be non-zero")]
    InvalidForegroundFence,
    #[error("foreground epoch {requested} is not newer than current epoch {current}")]
    StaleForegroundEpoch { current: u64, requested: u64 },
}

#[cfg(test)]
mod tests;
