use remagic_core::{AppId, AppToken};
use remagic_protocol::{
    Envelope, LifecycleCommand, LifecycleCommandEnvelope, LifecycleEvent, LifecycleEventBody,
    LifecycleEventEnvelope, LifecycleStage, MAX_FRAME,
};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

pub struct LifecycleClient {
    socket: OwnedFd,
    app_id: AppId,
    active_token: Option<AppToken>,
    request_sequence: u64,
}

impl LifecycleClient {
    pub fn from_inherited_fd(app_id: AppId, descriptor: i32) -> Result<Self, LifecycleError> {
        if descriptor < 0 {
            return Err(LifecycleError::InvalidDescriptor(descriptor));
        }
        let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, 3) };
        if duplicate < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let socket = unsafe { OwnedFd::from_raw_fd(duplicate) };
        if socket_type(socket.as_raw_fd())? != libc::SOCK_SEQPACKET {
            return Err(LifecycleError::WrongSocketType);
        }
        set_nonblocking(socket.as_raw_fd())?;
        Ok(Self {
            socket,
            app_id,
            active_token: None,
            request_sequence: 0,
        })
    }

    pub fn poll(&mut self) -> Result<Vec<LifecycleCommand>, LifecycleError> {
        let mut commands = Vec::new();
        loop {
            let mut packet = vec![0_u8; MAX_FRAME + 4];
            let read = unsafe {
                libc::recv(
                    self.socket.as_raw_fd(),
                    packet.as_mut_ptr().cast(),
                    packet.len(),
                    0,
                )
            };
            if read == 0 {
                return Err(LifecycleError::Disconnected);
            }
            if read < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            packet.truncate(read as usize);
            let envelope = decode_command_packet(&packet)?;
            envelope.validate_header()?;
            envelope.body.validate()?;
            self.accept_token(&envelope)?;
            commands.push(envelope.body.command);
        }
        Ok(commands)
    }

    pub fn ready(&mut self, first_frame_sequence: u64) -> Result<(), LifecycleError> {
        if first_frame_sequence == 0 {
            return Err(LifecycleError::ZeroFrameSequence);
        }
        self.send(LifecycleEvent::Ready {
            first_frame_sequence: Some(first_frame_sequence),
        })
    }

    pub fn background_ready(
        &mut self,
        title: impl Into<String>,
        subtitle: impl Into<String>,
    ) -> Result<(), LifecycleError> {
        self.send(LifecycleEvent::StateSaved {
            resume_payload: None,
        })?;
        self.send(LifecycleEvent::BackgroundReady {
            title: title.into(),
            subtitle: subtitle.into(),
            resume_payload: None,
        })
    }

    pub fn shutdown_complete(&mut self, exit_code: i32) -> Result<(), LifecycleError> {
        self.send(LifecycleEvent::ShutdownComplete { exit_code })
    }

    pub fn failed(
        &mut self,
        stage: LifecycleStage,
        message: impl Into<String>,
        retryable: bool,
    ) -> Result<(), LifecycleError> {
        self.send(LifecycleEvent::Failed {
            stage,
            message: message.into(),
            retryable,
        })
    }

    pub fn active_token(&self) -> Option<&AppToken> {
        self.active_token.as_ref()
    }

    fn accept_token(&mut self, envelope: &LifecycleCommandEnvelope) -> Result<(), LifecycleError> {
        let token = &envelope.body.token;
        if token.app_id != self.app_id || token.generation == 0 || token.lease_id == Some(0) {
            return Err(LifecycleError::InvalidToken);
        }
        match &self.active_token {
            None if matches!(envelope.body.command, LifecycleCommand::Start { .. })
                && token.foreground_epoch > 0
                && token.lease_id.is_some() => {}
            None => return Err(LifecycleError::StartRequired),
            Some(active)
                if token.generation != active.generation
                    || token.foreground_epoch < active.foreground_epoch =>
            {
                return Err(LifecycleError::StaleToken)
            }
            Some(_) => {}
        }
        self.active_token = Some(token.clone());
        Ok(())
    }

    fn send(&mut self, event: LifecycleEvent) -> Result<(), LifecycleError> {
        let token = self
            .active_token
            .clone()
            .ok_or(LifecycleError::StartRequired)?;
        self.request_sequence = self.request_sequence.saturating_add(1);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let envelope = Envelope::new(
            format!("{}-{stamp}-{}", self.app_id, self.request_sequence),
            LifecycleEventBody { token, event },
        );
        send_event_packet(self.socket.as_raw_fd(), &envelope)
    }
}

fn decode_command_packet(packet: &[u8]) -> Result<LifecycleCommandEnvelope, LifecycleError> {
    if packet.len() < 4 {
        return Err(LifecycleError::InvalidFrame(packet.len()));
    }
    let length = u32::from_be_bytes(packet[..4].try_into().unwrap()) as usize;
    if length == 0 || length > MAX_FRAME || packet.len() != length + 4 {
        return Err(LifecycleError::InvalidFrame(length));
    }
    Ok(serde_json::from_slice(&packet[4..])?)
}

fn encode_event_packet(envelope: &LifecycleEventEnvelope) -> Result<Vec<u8>, LifecycleError> {
    envelope.validate_header()?;
    envelope.body.validate()?;
    let payload = serde_json::to_vec(envelope)?;
    if payload.is_empty() || payload.len() > MAX_FRAME {
        return Err(LifecycleError::InvalidFrame(payload.len()));
    }
    let mut packet = Vec::with_capacity(payload.len() + 4);
    packet.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    packet.extend_from_slice(&payload);
    Ok(packet)
}

fn send_event_packet(fd: i32, envelope: &LifecycleEventEnvelope) -> Result<(), LifecycleError> {
    let packet = encode_event_packet(envelope)?;
    let deadline = Instant::now() + Duration::from_millis(2_000);
    loop {
        let sent =
            unsafe { libc::send(fd, packet.as_ptr().cast(), packet.len(), libc::MSG_NOSIGNAL) };
        if sent == packet.len() as isize {
            return Ok(());
        }
        if sent >= 0 {
            return Err(LifecycleError::ShortWrite(sent as usize));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() == io::ErrorKind::WouldBlock && wait_writable(fd, deadline)? {
            continue;
        }
        return Err(error.into());
    }
}

fn wait_writable(fd: i32, deadline: Instant) -> io::Result<bool> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Ok(false);
    }
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLOUT | libc::POLLERR | libc::POLLHUP,
        revents: 0,
    };
    let timeout = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
    loop {
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout) };
        if result >= 0 {
            return Ok(result > 0 && descriptor.revents & libc::POLLOUT != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn socket_type(fd: i32) -> io::Result<i32> {
    let mut kind: libc::c_int = 0;
    let mut length = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut kind as *mut libc::c_int).cast(),
            &mut length,
        )
    };
    if result == 0 {
        Ok(kind)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn set_nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum LifecycleError {
    #[error("invalid lifecycle descriptor {0}")]
    InvalidDescriptor(i32),
    #[error("lifecycle channel disconnected")]
    Disconnected,
    #[error("lifecycle descriptor is not a SOCK_SEQPACKET socket")]
    WrongSocketType,
    #[error("invalid lifecycle frame length {0}")]
    InvalidFrame(usize),
    #[error("lifecycle channel made a short packet write of {0} bytes")]
    ShortWrite(usize),
    #[error("lifecycle must begin with a fenced start command")]
    StartRequired,
    #[error("lifecycle token is invalid for this application")]
    InvalidToken,
    #[error("stale lifecycle token")]
    StaleToken,
    #[error("first frame sequence must be non-zero")]
    ZeroFrameSequence,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Header(#[from] remagic_protocol::ProtocolValidationError),
    #[error(transparent)]
    Validation(#[from] remagic_protocol::LifecycleValidationError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use remagic_core::AppToken;

    #[test]
    fn event_frame_round_trips_with_length_prefix() {
        let body = LifecycleEventBody {
            token: AppToken {
                app_id: AppId::new("upload").unwrap(),
                generation: 1,
                foreground_epoch: 2,
                lease_id: Some(3),
            },
            event: LifecycleEvent::Ready {
                first_frame_sequence: Some(4),
            },
        };
        let envelope = Envelope::new("upload-test-1", body);
        let packet = encode_event_packet(&envelope).unwrap();
        let length = u32::from_be_bytes(packet[..4].try_into().unwrap()) as usize;
        assert_eq!(packet.len(), length + 4);
        let decoded: LifecycleEventEnvelope = serde_json::from_slice(&packet[4..]).unwrap();
        assert_eq!(decoded, envelope);
    }

    #[test]
    fn malformed_frames_are_rejected_before_json() {
        assert!(matches!(
            decode_command_packet(b"\0\0\0\x08{}"),
            Err(LifecycleError::InvalidFrame(8))
        ));
    }
}
