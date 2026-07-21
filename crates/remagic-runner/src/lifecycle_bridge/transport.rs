use super::BridgeError;
use remagic_core::AppId;
use remagic_protocol::{LifecycleCommandEnvelope, MAX_FRAME};
use serde_json::Value;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use tokio::io::unix::AsyncFd;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ChildTransport {
    LengthPrefixed,
    Newline,
}

impl ChildTransport {
    pub(super) fn for_app(app_id: &AppId) -> Self {
        if app_id.as_str() == "magicpaper" {
            Self::LengthPrefixed
        } else {
            // KOReader's Lua adapter consumes one JSON envelope per line.
            Self::Newline
        }
    }
}

pub(super) fn encode_command(
    transport: ChildTransport,
    envelope: &LifecycleCommandEnvelope,
) -> Result<Vec<u8>, BridgeError> {
    let payload = match transport {
        ChildTransport::LengthPrefixed => serde_json::to_vec(envelope)?,
        ChildTransport::Newline => encode_flat_command(envelope)?,
    };
    if payload.is_empty() || payload.len() > MAX_FRAME {
        return Err(BridgeError::FrameLength(payload.len()));
    }
    Ok(frame_payload(transport, payload))
}

fn encode_flat_command(envelope: &LifecycleCommandEnvelope) -> Result<Vec<u8>, BridgeError> {
    let mut value = serde_json::to_value(envelope)?;
    let body = value
        .get_mut("body")
        .and_then(Value::as_object_mut)
        .ok_or(BridgeError::InvalidEnvelope("missing command body"))?;
    let token = body
        .remove("token")
        .and_then(|value| value.as_object().cloned())
        .ok_or(BridgeError::InvalidEnvelope("missing command token"))?;
    body.extend(token);
    Ok(serde_json::to_vec(&value)?)
}

fn frame_payload(transport: ChildTransport, payload: Vec<u8>) -> Vec<u8> {
    let mut packet = Vec::with_capacity(payload.len() + 4);
    match transport {
        ChildTransport::LengthPrefixed => {
            packet.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            packet.extend_from_slice(&payload);
        }
        ChildTransport::Newline => {
            packet.extend_from_slice(&payload);
            packet.push(b'\n');
        }
    }
    packet
}

pub(super) fn decode_packet(
    transport: ChildTransport,
    packet: &[u8],
) -> Result<Vec<Vec<u8>>, BridgeError> {
    match transport {
        ChildTransport::LengthPrefixed => decode_length_prefixed(packet),
        ChildTransport::Newline => decode_newline(packet),
    }
}

fn decode_length_prefixed(packet: &[u8]) -> Result<Vec<Vec<u8>>, BridgeError> {
    if packet.len() < 4 {
        return Err(BridgeError::InvalidEnvelope("truncated frame header"));
    }
    let length = u32::from_be_bytes(packet[..4].try_into().unwrap()) as usize;
    if length == 0 || length > MAX_FRAME || packet.len() != length + 4 {
        return Err(BridgeError::FrameLength(length));
    }
    Ok(vec![packet[4..].to_vec()])
}

fn decode_newline(packet: &[u8]) -> Result<Vec<Vec<u8>>, BridgeError> {
    if packet.len() > MAX_FRAME + 1 {
        return Err(BridgeError::FrameLength(packet.len()));
    }
    if !packet.ends_with(b"\n") {
        return Err(BridgeError::InvalidEnvelope(
            "unterminated newline lifecycle frame",
        ));
    }
    let payloads = packet
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line).to_vec())
        .collect::<Vec<_>>();
    if payloads.is_empty() {
        return Err(BridgeError::InvalidEnvelope("empty newline frame"));
    }
    if payloads.iter().any(|payload| payload.len() > MAX_FRAME) {
        return Err(BridgeError::FrameLength(MAX_FRAME + 1));
    }
    Ok(payloads)
}

pub(super) async fn send_packet(descriptor: &AsyncFd<OwnedFd>, packet: &[u8]) -> io::Result<()> {
    loop {
        let mut readiness = descriptor.writable().await?;
        match readiness.try_io(|inner| send_once(inner.as_raw_fd(), packet)) {
            Ok(result) => return result,
            Err(_) => continue,
        }
    }
}

fn send_once(descriptor: i32, packet: &[u8]) -> io::Result<()> {
    let written = unsafe {
        libc::send(
            descriptor,
            packet.as_ptr().cast(),
            packet.len(),
            libc::MSG_NOSIGNAL,
        )
    };
    if written < 0 {
        Err(io::Error::last_os_error())
    } else if written as usize != packet.len() {
        Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short lifecycle packet",
        ))
    } else {
        Ok(())
    }
}

pub(super) async fn receive_packet(descriptor: &AsyncFd<OwnedFd>) -> io::Result<Vec<u8>> {
    let mut packet = vec![0_u8; MAX_FRAME + 5];
    loop {
        let mut readiness = descriptor.readable().await?;
        match readiness.try_io(|inner| receive_once(inner.as_raw_fd(), &mut packet)) {
            Ok(result) => {
                packet.truncate(result?);
                return Ok(packet);
            }
            Err(_) => continue,
        }
    }
}

fn receive_once(descriptor: i32, packet: &mut [u8]) -> io::Result<usize> {
    let received = unsafe { libc::recv(descriptor, packet.as_mut_ptr().cast(), packet.len(), 0) };
    if received < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(received as usize)
    }
}
