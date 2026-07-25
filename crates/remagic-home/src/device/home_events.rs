//! Event-driven requests from the ReMagic supervisor to the Home surface.

use std::fs;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::Path;
use std::time::Duration;

const SOCKET_PATH: &str = "/run/remagic/home-events.sock";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Event {
    AutoSleep,
    CoverSleep { app_id: Option<remagic_core::AppId> },
    ResumeUnlock { app_id: Option<remagic_core::AppId> },
    WallpapersChanged,
}

pub(super) struct Receiver {
    socket: UnixDatagram,
}

impl Receiver {
    pub(super) fn bind() -> io::Result<Self> {
        let _ = fs::remove_file(SOCKET_PATH);
        let socket = UnixDatagram::bind(SOCKET_PATH)?;
        socket.set_nonblocking(true)?;
        fs::set_permissions(SOCKET_PATH, fs::Permissions::from_mode(0o600))?;
        Ok(Self { socket })
    }

    pub(super) fn drain(&self) -> io::Result<Vec<Event>> {
        let mut events = Vec::new();
        let mut packet = [0_u8; 64];
        loop {
            match self.socket.recv(&mut packet) {
                Ok(length) if &packet[..length] == b"auto_sleep\n" => events.push(Event::AutoSleep),
                Ok(length) if &packet[..length] == b"resume_unlock\n" => {
                    events.push(Event::ResumeUnlock { app_id: None })
                }
                Ok(length) if &packet[..length] == b"cover_sleep\n" => {
                    events.push(Event::CoverSleep { app_id: None })
                }
                Ok(length) if &packet[..length] == b"cover_open\n" => {
                    events.push(Event::ResumeUnlock { app_id: None })
                }
                Ok(length) if &packet[..length] == b"wallpapers_changed\n" => {
                    events.push(Event::WallpapersChanged)
                }
                Ok(length) => {
                    if let Some(event) = parse_event(&packet[..length]) {
                        events.push(event);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(events)
    }

    pub(super) fn wait_with_input(
        &self,
        input_fd: RawFd,
        timeout: Option<Duration>,
    ) -> io::Result<()> {
        let mut descriptors = [
            libc::pollfd {
                fd: input_fd,
                events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
                revents: 0,
            },
            libc::pollfd {
                fd: self.socket.as_raw_fd(),
                events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
                revents: 0,
            },
        ];
        let timeout_ms = timeout.map_or(-1, |duration| {
            duration.as_millis().clamp(1, i32::MAX as u128) as i32
        });
        loop {
            let result = unsafe {
                libc::poll(
                    descriptors.as_mut_ptr(),
                    descriptors.len() as libc::nfds_t,
                    timeout_ms,
                )
            };
            if result >= 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

fn parse_event(bytes: &[u8]) -> Option<Event> {
    let text = std::str::from_utf8(bytes).ok()?.trim_end_matches('\n');
    let (command, value) = text.split_once(' ')?;
    let app_id = remagic_core::AppId::new(value.to_owned()).ok()?;
    match command {
        "cover_sleep_app" => Some(Event::CoverSleep {
            app_id: Some(app_id),
        }),
        "cover_open_app" | "resume_unlock_app" => Some(Event::ResumeUnlock {
            app_id: Some(app_id),
        }),
        _ => None,
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let _ = fs::remove_file(Path::new(SOCKET_PATH));
    }
}
