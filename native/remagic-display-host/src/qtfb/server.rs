use super::queue::{InputQueue, INPUT_QUEUE_CAPACITY};
use super::state::HostState;
use crate::protocol::{
    initialize_reply, ClientPacket, QTFB_CLIENT_MESSAGE_SIZE, QTFB_SERVER_MESSAGE_SIZE, QTFB_SOCKET,
};
use std::collections::HashSet;
use std::ffi::CString;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const SOCKET_BACKLOG: i32 = 16;

pub struct QtfbServer {
    state: Arc<HostState>,
    stop: Arc<AtomicBool>,
    listener: RawFd,
    active_clients: Arc<Mutex<HashSet<RawFd>>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl QtfbServer {
    pub fn start(state: Arc<HostState>, path: &str) -> io::Result<Self> {
        let path = CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket path contains NUL"))?;
        let listener = create_listener(&path)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_state = Arc::clone(&state);
        let active_clients = Arc::new(Mutex::new(HashSet::new()));
        let thread_clients = Arc::clone(&active_clients);
        let socket_path = path.clone();
        let thread = std::thread::Builder::new()
            .name("remagic-qtfb".into())
            .spawn(move || {
                accept_loop(
                    listener,
                    thread_state,
                    thread_stop,
                    thread_clients,
                    socket_path,
                )
            })?;
        Ok(Self {
            state,
            stop,
            listener,
            active_clients,
            thread: Some(thread),
        })
    }

    pub fn default(state: Arc<HostState>) -> io::Result<Self> {
        Self::start(state, QTFB_SOCKET)
    }
}

impl Drop for QtfbServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        unsafe { libc::shutdown(self.listener, libc::SHUT_RDWR) };
        for fd in self.active_clients.lock().unwrap().iter() {
            unsafe { libc::shutdown(*fd, libc::SHUT_RDWR) };
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = &self.state;
    }
}

fn create_listener(path: &CString) -> io::Result<RawFd> {
    let listener =
        unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if listener < 0 {
        return Err(io::Error::last_os_error());
    }
    unsafe { libc::unlink(path.as_ptr()) };
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_bytes_with_nul();
    if bytes.len() > address.sun_path.len() {
        unsafe { libc::close(listener) };
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path too long",
        ));
    }
    for (target, source) in address.sun_path.iter_mut().zip(bytes.iter().copied()) {
        *target = source as libc::c_char;
    }
    let bind_result = unsafe {
        libc::bind(
            listener,
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if bind_result != 0 || unsafe { libc::listen(listener, SOCKET_BACKLOG) } != 0 {
        let error = io::Error::last_os_error();
        unsafe {
            libc::close(listener);
            libc::unlink(path.as_ptr());
        }
        return Err(error);
    }
    unsafe { libc::chmod(path.as_ptr(), libc::S_IRUSR | libc::S_IWUSR) };
    Ok(listener)
}

fn accept_loop(
    listener: RawFd,
    state: Arc<HostState>,
    stop: Arc<AtomicBool>,
    active_clients: Arc<Mutex<HashSet<RawFd>>>,
    path: CString,
) {
    let mut clients = Vec::new();
    while !stop.load(Ordering::Acquire) && !state.is_shutdown() {
        let mut descriptor = libc::pollfd {
            fd: listener,
            events: libc::POLLIN,
            revents: 0,
        };
        if unsafe { libc::poll(&mut descriptor, 1, -1) } <= 0 {
            continue;
        }
        let fd = unsafe {
            libc::accept4(
                listener,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC,
            )
        };
        if fd >= 0 {
            spawn_client(
                fd,
                Arc::clone(&state),
                Arc::clone(&active_clients),
                &mut clients,
            );
        }
    }
    unsafe {
        libc::close(listener);
        libc::unlink(path.as_ptr());
    }
    for client in clients {
        let _ = client.join();
    }
}

fn spawn_client(
    fd: RawFd,
    state: Arc<HostState>,
    active_clients: Arc<Mutex<HashSet<RawFd>>>,
    clients: &mut Vec<std::thread::JoinHandle<()>>,
) {
    active_clients.lock().unwrap().insert(fd);
    let client_set = Arc::clone(&active_clients);
    clients.push(std::thread::spawn(move || {
        handle_client(fd, state);
        client_set.lock().unwrap().remove(&fd);
    }));
}

fn handle_client(fd: RawFd, state: Arc<HostState>) {
    let input_queue = InputQueue::new(INPUT_QUEUE_CAPACITY);
    let Some(writer) = spawn_writer(fd, Arc::clone(&input_queue)) else {
        unsafe { libc::close(fd) };
        return;
    };
    let mut key = None;
    while let Some(packet) = receive_packet(fd) {
        match process_packet(fd, &state, &input_queue, &mut key, packet) {
            Ok(true) => {}
            Ok(false) | Err(_) => break,
        }
    }
    state.unregister(fd, key);
    unsafe {
        libc::shutdown(fd, libc::SHUT_RDWR);
        libc::close(fd);
    }
    input_queue.close();
    let _ = writer.join();
}

fn spawn_writer(fd: RawFd, input_queue: Arc<InputQueue>) -> Option<std::thread::JoinHandle<()>> {
    let writer_fd = unsafe { libc::dup(fd) };
    if writer_fd < 0 {
        return None;
    }
    Some(std::thread::spawn(move || {
        while let Some(packet) = input_queue.pop() {
            if send_packet(writer_fd, &packet).is_err() {
                break;
            }
        }
        unsafe { libc::close(writer_fd) };
    }))
}

fn receive_packet(fd: RawFd) -> Option<ClientPacket> {
    let mut bytes = [0_u8; QTFB_CLIENT_MESSAGE_SIZE];
    let count = unsafe { libc::recv(fd, bytes.as_mut_ptr().cast(), bytes.len(), 0) };
    if count != bytes.len() as isize {
        return None;
    }
    ClientPacket::decode(&bytes).ok()
}

fn process_packet(
    fd: RawFd,
    state: &HostState,
    input_queue: &Arc<InputQueue>,
    key: &mut Option<i32>,
    packet: ClientPacket,
) -> io::Result<bool> {
    match packet {
        ClientPacket::Initialize {
            key: requested,
            format,
            width,
            height,
        } if key.is_none() => {
            let surface = state.register(requested, width, height, format)?;
            if let Err(error) = send_initialize_reply(fd, surface.shm_key, surface.len) {
                state.abort_registration(requested);
                return Err(error);
            }
            if let Err(error) = state.activate_client(requested, fd, Arc::clone(input_queue)) {
                state.abort_registration(requested);
                return Err(error);
            }
            *key = Some(requested);
            Ok(true)
        }
        ClientPacket::Initialize { .. } => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "connection initialized twice",
        )),
        ClientPacket::Update { rect } => {
            state.commit_damage(require_key(*key)?, rect)?;
            Ok(true)
        }
        ClientPacket::SetRefreshMode(mode) => {
            state.set_refresh_mode(require_key(*key)?, mode)?;
            Ok(true)
        }
        ClientPacket::RequestFullRefresh => {
            state.request_surface_full_refresh(require_key(*key)?)?;
            Ok(true)
        }
        ClientPacket::Terminate => Ok(false),
    }
}

fn require_key(key: Option<i32>) -> io::Result<i32> {
    key.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "connection is not initialized"))
}

fn send_initialize_reply(fd: RawFd, shm_key: i32, len: usize) -> io::Result<()> {
    let reply = initialize_reply(shm_key, len);
    let count = unsafe { libc::send(fd, reply.as_ptr().cast(), reply.len(), libc::MSG_NOSIGNAL) };
    if count == reply.len() as isize {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn send_packet(fd: RawFd, packet: &[u8; QTFB_SERVER_MESSAGE_SIZE]) -> io::Result<()> {
    loop {
        let sent =
            unsafe { libc::send(fd, packet.as_ptr().cast(), packet.len(), libc::MSG_NOSIGNAL) };
        if sent == packet.len() as isize {
            return Ok(());
        }
        if sent < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "short QTFB input packet",
        ));
    }
}
