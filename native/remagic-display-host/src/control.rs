use crate::protocol::{ControlEnvelope, ControlReply, DisplayControl, DISPLAY_CONTROL_SOCKET};
use crate::qtfb::HostState;
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct ControlServer {
    stop: Arc<AtomicBool>,
    path: String,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ControlServer {
    pub fn start(state: Arc<HostState>, path: impl Into<String>) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = Path::new(&path).parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_path = path.clone();
        let thread = std::thread::Builder::new()
            .name("remagic-display-control".into())
            .spawn(move || {
                while !thread_stop.load(Ordering::Acquire) && !state.is_shutdown() {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let _ = handle(stream, &state);
                        }
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
                let _ = fs::remove_file(thread_path);
            })?;
        Ok(Self {
            stop,
            path,
            thread: Some(thread),
        })
    }

    pub fn default(state: Arc<HostState>) -> io::Result<Self> {
        Self::start(state, DISPLAY_CONTROL_SOCKET)
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn handle(mut stream: UnixStream, state: &HostState) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    let request: ControlEnvelope = serde_json::from_str(&line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let reply = process_request(request, state);
    let encoded = serde_json::to_vec(&reply).map_err(io::Error::other)?;
    stream.write_all(&encoded)?;
    stream.write_all(b"\n")?;
    stream.flush()
}

fn process_request(request: ControlEnvelope, state: &HostState) -> ControlReply {
    let mut reply = ControlReply {
        protocol: 1,
        request_id: request.request_id,
        ok: true,
        status: "ok".into(),
        error: None,
        snapshot: None,
    };
    if request.protocol != 1 {
        reply.ok = false;
        reply.status = "unsupported_protocol".into();
        reply.error = Some(format!(
            "display protocol {} is not supported",
            request.protocol
        ));
    } else {
        let result = dispatch(request.command, state, &mut reply);
        if let Err(error) = result {
            reply.ok = false;
            reply.status = "error".into();
            reply.error = Some(error.to_string());
        }
    }
    reply
}

fn dispatch(
    command: DisplayControl,
    state: &HostState,
    reply: &mut ControlReply,
) -> io::Result<()> {
    match command {
        DisplayControl::Status => {
            reply.snapshot = Some(state.snapshot());
            Ok(())
        }
        DisplayControl::SetForeground {
            key,
            generation,
            foreground_epoch,
            full_refresh,
        } => state.set_foreground(key, generation, foreground_epoch, full_refresh),
        DisplayControl::PrepareForeground {
            key,
            generation,
            foreground_epoch,
        } => state.prepare_foreground(key, generation, foreground_epoch),
        DisplayControl::ActivateForeground {
            key,
            generation,
            foreground_epoch,
            ink_enabled,
            full_refresh,
        } => {
            state.activate_foreground(key, generation, foreground_epoch, ink_enabled, full_refresh)
        }
        DisplayControl::ClearForeground => state.clear_foreground(),
        DisplayControl::ConfigureInk {
            key,
            generation,
            foreground_epoch,
            enabled,
            region,
        } => state.configure_ink(key, generation, foreground_epoch, enabled, region),
        DisplayControl::RequestFullRefresh => state.request_full_refresh(),
        DisplayControl::ShowLock {
            key,
            generation,
            foreground_epoch,
            sleep_epoch,
            unlock_region,
        } => state.show_lock(
            key,
            generation,
            foreground_epoch,
            sleep_epoch,
            unlock_region,
        ),
        DisplayControl::RefreshLock { sleep_epoch } => state.refresh_lock(sleep_epoch),
        DisplayControl::CancelLock {
            sleep_epoch,
            replacement_surface_sequence,
        } => state.cancel_lock(sleep_epoch, replacement_surface_sequence),
        DisplayControl::InjectTap { x, y } => state.inject_tap(x, y),
        DisplayControl::InjectPenLine {
            x0,
            y0,
            x1,
            y1,
            points,
        } => state.inject_pen_line(x0, y0, x1, y1, points),
        DisplayControl::Shutdown => {
            state.shutdown();
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qtfb::HostState;
    use std::sync::mpsc;

    #[test]
    fn status_returns_physical_geometry() {
        let (tx, _rx) = mpsc::sync_channel(1024);
        let state = HostState::new(tx, 960, 1696, 3840);
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || handle(server, &state).unwrap());
        client
            .write_all(b"{\"protocol\":1,\"request_id\":\"t\",\"command\":\"status\"}\n")
            .unwrap();
        let mut response = String::new();
        BufReader::new(client).read_line(&mut response).unwrap();
        let response: ControlReply = serde_json::from_str(&response).unwrap();
        assert!(response.ok);
        assert_eq!(response.snapshot.unwrap().physical_width, 960);
        worker.join().unwrap();
    }
}
