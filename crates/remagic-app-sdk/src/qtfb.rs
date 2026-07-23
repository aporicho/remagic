use remagic_device::DeviceDisplayProfile;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

const MESSAGE_INITIALIZE: u8 = 0;
const MESSAGE_UPDATE: u8 = 1;
const MESSAGE_TERMINATE: u8 = 3;
const MESSAGE_USERINPUT: u8 = 4;
const MESSAGE_SET_REFRESH_MODE: u8 = 5;
const UPDATE_ALL: i32 = 0;
const UPDATE_PARTIAL: i32 = 1;
const MAX_EVENTS_PER_PUMP: usize = 256;

pub const REFRESH_FAST: i32 = 1;
pub const REFRESH_UI: i32 = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TouchPhase {
    Press,
    Move,
    Release,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputEvent {
    pub phase: TouchPhase,
    pub finger: i32,
    pub x: i32,
    pub y: i32,
}

pub struct QtfbClient {
    socket: RawFd,
    shared: *mut u8,
    shared_len: usize,
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    refresh_mode: i32,
}

unsafe impl Send for QtfbClient {}

impl QtfbClient {
    pub fn connect(
        socket_path: &Path,
        key: i32,
        display: &DeviceDisplayProfile,
    ) -> Result<Self, QtfbError> {
        let socket = connect_socket(socket_path)?;
        let (shared_key, shared_len) = initialize(socket.as_raw_fd(), key, display.qtfb_format)?;
        let required = display
            .stride
            .checked_mul(display.logical_height as usize)
            .ok_or(QtfbError::SurfaceOverflow)?;
        let shared = map_framebuffer(shared_key, shared_len, required)?;
        set_nonblocking(socket.as_raw_fd())?;
        Ok(Self {
            socket: socket.into_raw_fd(),
            shared,
            shared_len,
            width: display.logical_width as usize,
            height: display.logical_height as usize,
            stride: display.stride,
            refresh_mode: REFRESH_UI,
        })
    }

    pub fn framebuffer(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.shared, self.shared_len) }
    }

    pub fn update_all(&mut self, refresh_mode: i32) -> Result<(), QtfbError> {
        self.set_refresh_mode(refresh_mode)?;
        let mut message = [0_u8; 24];
        message[0] = MESSAGE_UPDATE;
        message[4..8].copy_from_slice(&UPDATE_ALL.to_le_bytes());
        send_packet(self.socket, &message)?;
        Ok(())
    }

    pub fn update_partial(
        &mut self,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
        refresh_mode: i32,
    ) -> Result<(), QtfbError> {
        self.set_refresh_mode(refresh_mode)?;
        let mut message = [0_u8; 24];
        message[0] = MESSAGE_UPDATE;
        message[4..8].copy_from_slice(&UPDATE_PARTIAL.to_le_bytes());
        for (slot, value) in [x, y, width, height].into_iter().enumerate() {
            let offset = 8 + slot * 4;
            message[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        send_packet(self.socket, &message)?;
        Ok(())
    }

    pub fn drain_touch(&self) -> Result<Vec<InputEvent>, QtfbError> {
        let mut events = Vec::new();
        loop {
            let mut packet = [0_u8; 32];
            let read =
                unsafe { libc::recv(self.socket, packet.as_mut_ptr().cast(), packet.len(), 0) };
            if read == 0 {
                return Err(QtfbError::Disconnected);
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
            if packet[0] == MESSAGE_USERINPUT && read >= 28 {
                let raw = i32::from_le_bytes(packet[8..12].try_into().unwrap());
                let phase = match raw {
                    0x10 => Some(TouchPhase::Press),
                    0x11 => Some(TouchPhase::Release),
                    0x12 => Some(TouchPhase::Move),
                    _ => None,
                };
                if let Some(phase) = phase {
                    events.push(InputEvent {
                        phase,
                        finger: i32::from_le_bytes(packet[12..16].try_into().unwrap()),
                        x: i32::from_le_bytes(packet[16..20].try_into().unwrap()),
                        y: i32::from_le_bytes(packet[20..24].try_into().unwrap()),
                    });
                }
                if events.len() == MAX_EVENTS_PER_PUMP {
                    break;
                }
            }
        }
        Ok(events)
    }

    pub fn wait(&self, timeout: Duration) -> Result<(), QtfbError> {
        let mut descriptor = libc::pollfd {
            fd: self.socket,
            events: libc::POLLIN | libc::POLLERR | libc::POLLHUP,
            revents: 0,
        };
        let timeout_ms = timeout.as_millis().clamp(0, i32::MAX as u128) as i32;
        loop {
            let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
            if result >= 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error.into());
            }
        }
    }

    fn set_refresh_mode(&mut self, mode: i32) -> Result<(), QtfbError> {
        if self.refresh_mode == mode {
            return Ok(());
        }
        let mut message = [0_u8; 24];
        message[0] = MESSAGE_SET_REFRESH_MODE;
        message[4..8].copy_from_slice(&mode.to_le_bytes());
        send_packet(self.socket, &message)?;
        self.refresh_mode = mode;
        Ok(())
    }
}

impl Drop for QtfbClient {
    fn drop(&mut self) {
        let mut message = [0_u8; 24];
        message[0] = MESSAGE_TERMINATE;
        let _ = send_packet(self.socket, &message);
        unsafe {
            libc::munmap(self.shared.cast(), self.shared_len);
            libc::close(self.socket);
        }
    }
}

fn connect_socket(path: &Path) -> Result<OwnedFd, QtfbError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.len() >= address.sun_path.len() {
        return Err(QtfbError::InvalidSocketPath);
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address.sun_path.iter_mut().zip(bytes) {
        *target = *source as libc::c_char;
    }
    let descriptor = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let socket = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let result = unsafe {
        libc::connect(
            socket.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(socket)
}

fn initialize(fd: RawFd, key: i32, format: u8) -> Result<(i32, usize), QtfbError> {
    let mut message = [0_u8; 24];
    message[0] = MESSAGE_INITIALIZE;
    message[4..8].copy_from_slice(&key.to_le_bytes());
    message[8] = format;
    send_packet(fd, &message)?;
    let mut reply = [0_u8; 32];
    let read = unsafe { libc::recv(fd, reply.as_mut_ptr().cast(), reply.len(), 0) };
    if read < 24 {
        return Err(QtfbError::InitializationRejected);
    }
    Ok((
        i32::from_le_bytes(reply[8..12].try_into().unwrap()),
        u64::from_le_bytes(reply[16..24].try_into().unwrap()) as usize,
    ))
}

fn map_framebuffer(key: i32, length: usize, required: usize) -> Result<*mut u8, QtfbError> {
    if length < required {
        return Err(QtfbError::SurfaceTooSmall { length, required });
    }
    let name = format!("/dev/shm/qtfb_{key}\0");
    let descriptor = unsafe { libc::open(name.as_ptr().cast(), libc::O_RDWR) };
    if descriptor < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let shared = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            descriptor,
            0,
        )
    };
    unsafe { libc::close(descriptor) };
    if shared == libc::MAP_FAILED {
        return Err(io::Error::last_os_error().into());
    }
    Ok(shared.cast())
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn send_packet(fd: RawFd, packet: &[u8]) -> Result<(), QtfbError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        let sent =
            unsafe { libc::send(fd, packet.as_ptr().cast(), packet.len(), libc::MSG_NOSIGNAL) };
        if sent == packet.len() as isize {
            return Ok(());
        }
        if sent >= 0 {
            return Err(QtfbError::ShortWrite(sent as usize));
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

fn wait_writable(fd: RawFd, deadline: std::time::Instant) -> io::Result<bool> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
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

#[derive(Debug, Error)]
pub enum QtfbError {
    #[error("invalid QTFB socket path")]
    InvalidSocketPath,
    #[error("QTFB host rejected surface initialization")]
    InitializationRejected,
    #[error("surface size overflows usize")]
    SurfaceOverflow,
    #[error("QTFB shared surface is too small: {length} < {required}")]
    SurfaceTooSmall { length: usize, required: usize },
    #[error("QTFB socket disconnected")]
    Disconnected,
    #[error("QTFB packet made a short write of {0} bytes")]
    ShortWrite(usize),
    #[error(transparent)]
    Io(#[from] io::Error),
}
