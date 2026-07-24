use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const CLIENT_SIZE: usize = 24;
const SERVER_SIZE: usize = 32;
const MESSAGE_INITIALIZE: u8 = 0;
const MESSAGE_UPDATE: u8 = 1;
const MESSAGE_TERMINATE: u8 = 3;
const MESSAGE_USERINPUT: u8 = 4;
const MESSAGE_REQUEST_FULL_REFRESH: u8 = 6;
const UPDATE_ALL: i32 = 0;
const UPDATE_PARTIAL: i32 = 1;
const INPUT_TOUCH_PRESS: i32 = 0x10;
const INPUT_TOUCH_RELEASE: i32 = 0x11;
const UPDATE_SEND_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TouchEvent {
    Press { x: i32, y: i32 },
    Release { x: i32, y: i32 },
}

pub struct Client {
    fd: RawFd,
    ptr: *mut u8,
    len: usize,
    pub width: i32,
    pub height: i32,
    pub stride: usize,
    primary_touch: Option<i32>,
    last_touch: (i32, i32),
    commit_sequence: AtomicU64,
}

unsafe impl Send for Client {}

impl Client {
    pub fn connect() -> io::Result<Self> {
        let profile = remagic_core::DeviceProfile::detect().map_err(io::Error::other)?;
        let socket =
            std::env::var("REMAGIC_QTFB_SOCKET").unwrap_or_else(|_| "/tmp/qtfb.sock".into());
        let key = std::env::var("QTFB_KEY")
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
            .unwrap_or(remagic_core::REMAGIC_HOME_QTFB_KEY);
        let fd = connect_socket(&socket)?;
        let (raw, len) = match initialize_surface(fd, key, &profile.display) {
            Ok(surface) => surface,
            Err(error) => {
                unsafe { libc::close(fd) };
                return Err(error);
            }
        };
        if let Err(error) = set_nonblocking(fd) {
            unsafe {
                libc::munmap(raw, len);
                libc::close(fd);
            }
            return Err(error);
        }
        Ok(Self {
            fd,
            ptr: raw.cast(),
            len,
            width: profile.display.logical_width,
            height: profile.display.logical_height,
            stride: profile.display.stride,
            primary_touch: None,
            last_touch: (0, 0),
            commit_sequence: AtomicU64::new(0),
        })
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    pub fn pixels(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    pub fn update_all(&self) -> io::Result<()> {
        let mut packet = [0_u8; CLIENT_SIZE];
        packet[0] = MESSAGE_UPDATE;
        packet[4..8].copy_from_slice(&UPDATE_ALL.to_le_bytes());
        send_packet(self.fd, &packet)?;
        self.commit_sequence.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    pub fn update(&self, x: i32, y: i32, width: i32, height: i32) -> io::Result<()> {
        let mut packet = [0_u8; CLIENT_SIZE];
        packet[0] = MESSAGE_UPDATE;
        for (offset, value) in [
            (4, UPDATE_PARTIAL),
            (8, x),
            (12, y),
            (16, width),
            (20, height),
        ] {
            packet[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        send_packet(self.fd, &packet)?;
        self.commit_sequence.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    pub fn request_full_refresh(&self) -> io::Result<()> {
        let mut packet = [0_u8; CLIENT_SIZE];
        packet[0] = MESSAGE_REQUEST_FULL_REFRESH;
        send_packet(self.fd, &packet)
    }

    pub fn commit_sequence(&self) -> u64 {
        self.commit_sequence.load(Ordering::Acquire)
    }

    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    pub fn poll_touch_events(&mut self) -> io::Result<Vec<TouchEvent>> {
        let mut events = Vec::new();
        loop {
            let mut packet = [0_u8; SERVER_SIZE];
            let received =
                unsafe { libc::recv(self.fd, packet.as_mut_ptr().cast(), packet.len(), 0) };
            if received == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "QTFB closed",
                ));
            }
            if received < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if received != packet.len() as isize || packet[0] != MESSAGE_USERINPUT {
                continue;
            }
            let input_type = i32::from_le_bytes(packet[8..12].try_into().unwrap());
            let device_id = i32::from_le_bytes(packet[12..16].try_into().unwrap());
            let x = i32::from_le_bytes(packet[16..20].try_into().unwrap());
            let y = i32::from_le_bytes(packet[20..24].try_into().unwrap());
            if input_type == INPUT_TOUCH_PRESS && self.primary_touch.is_none() {
                self.primary_touch = Some(device_id);
                self.last_touch = (x, y);
                events.push(TouchEvent::Press { x, y });
            } else if self.primary_touch == Some(device_id) {
                self.last_touch = (x, y);
                if input_type == INPUT_TOUCH_RELEASE {
                    events.push(TouchEvent::Release {
                        x: self.last_touch.0,
                        y: self.last_touch.1,
                    });
                    self.primary_touch = None;
                }
            }
        }
        Ok(events)
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn wait_fd(fd: RawFd, events: i16, timeout: Duration) -> io::Result<()> {
    let timeout_ms = timeout.as_millis().clamp(1, i32::MAX as u128) as i32;
    let mut descriptor = libc::pollfd {
        fd,
        events: events | libc::POLLERR | libc::POLLHUP,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result >= 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn connect_socket(socket: &str) -> io::Result<RawFd> {
    let path = CString::new(socket)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "QTFB path contains NUL"))?;
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address
        .sun_path
        .iter_mut()
        .zip(path.as_bytes_with_nul().iter().copied())
    {
        *target = source as libc::c_char;
    }
    let result = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if result == 0 {
        Ok(fd)
    } else {
        let error = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        Err(error)
    }
}

fn initialize_surface(
    fd: RawFd,
    key: i32,
    display: &remagic_core::DeviceDisplayProfile,
) -> io::Result<(*mut libc::c_void, usize)> {
    let mut initialize = [0_u8; CLIENT_SIZE];
    initialize[0] = MESSAGE_INITIALIZE;
    initialize[4..8].copy_from_slice(&key.to_le_bytes());
    initialize[8] = display.qtfb_format;
    send_packet(fd, &initialize)?;

    let mut reply = [0_u8; SERVER_SIZE];
    let received = unsafe { libc::recv(fd, reply.as_mut_ptr().cast(), reply.len(), 0) };
    if received != reply.len() as isize || reply[0] != MESSAGE_INITIALIZE {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "QTFB rejected the manager surface",
        ));
    }
    let shm_key = i32::from_le_bytes(reply[8..12].try_into().unwrap());
    let len = u64::from_le_bytes(reply[16..24].try_into().unwrap()) as usize;
    let shm_name = CString::new(format!("/qtfb_{shm_key}")).unwrap();
    let shm_fd = unsafe { libc::shm_open(shm_name.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC, 0) };
    if shm_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let raw = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            shm_fd,
            0,
        )
    };
    unsafe { libc::close(shm_fd) };
    if raw == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    let minimum = display
        .stride
        .checked_mul(display.logical_height as usize)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "surface size overflow"))?;
    if len < minimum {
        unsafe { libc::munmap(raw, len) };
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "QTFB manager surface is too small",
        ));
    }
    Ok((raw, len))
}

impl Drop for Client {
    fn drop(&mut self) {
        let mut terminate = [0_u8; CLIENT_SIZE];
        terminate[0] = MESSAGE_TERMINATE;
        let _ = send_packet(self.fd, &terminate);
        unsafe {
            libc::munmap(self.ptr.cast(), self.len);
            libc::close(self.fd);
        }
    }
}

fn send_packet(fd: RawFd, packet: &[u8; CLIENT_SIZE]) -> io::Result<()> {
    let deadline = Instant::now() + UPDATE_SEND_TIMEOUT;
    loop {
        let sent =
            unsafe { libc::send(fd, packet.as_ptr().cast(), packet.len(), libc::MSG_NOSIGNAL) };
        if sent == packet.len() as isize {
            return Ok(());
        }
        if sent >= 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "short QTFB seqpacket send",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() != io::ErrorKind::WouldBlock {
            return Err(error);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "QTFB update remained backpressured for 250ms",
            ));
        }
        wait_fd(fd, libc::POLLOUT, deadline.saturating_duration_since(now))?;
    }
}
