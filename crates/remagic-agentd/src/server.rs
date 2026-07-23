use crate::requests::{cancel_connection_turn, dispatch, send_error, ConnectionTurn};
use crate::state::{AgentState, ClientIdentity};
use remagic_core::{AppId, DeviceProfile};
use remagic_protocol::{
    read_agent_frame, write_agent_frame, AgentErrorCode, AgentEvent, AgentFrameError,
    DEFAULT_AGENT_SOCKET,
};
use std::fs;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex};
use tracing::{info, warn};

const SYSTEMD_LISTEN_FD: RawFd = 3;
const IDLE_EXIT: Duration = Duration::from_secs(600);

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let state = AgentState::new(DeviceProfile::detect()?, resolve_pi_binary());
    let listener = activated_or_private_listener(Path::new(DEFAULT_AGENT_SOCKET))?;
    let clients = Arc::new(AtomicUsize::new(0));
    let activity = Arc::new(Mutex::new(Instant::now()));
    info!(socket = DEFAULT_AGENT_SOCKET, "Pi agent service ready");
    loop {
        match tokio::time::timeout(Duration::from_secs(60), listener.accept()).await {
            Ok(Ok((stream, _))) => {
                let Some(identity) = peer_identity(&stream) else {
                    warn!("rejected agent client outside a managed application cgroup");
                    continue;
                };
                clients.fetch_add(1, Ordering::AcqRel);
                *activity.lock().await = Instant::now();
                spawn_connection(stream, state.clone(), identity, &clients, &activity);
            }
            Ok(Err(error)) => return Err(error.into()),
            Err(_) if idle(&clients, &activity).await => {
                info!("Pi agent service idle; returning ownership to socket activation");
                return Ok(());
            }
            Err(_) => {}
        }
    }
}

fn spawn_connection(
    stream: UnixStream,
    state: AgentState,
    identity: ClientIdentity,
    clients: &Arc<AtomicUsize>,
    activity: &Arc<Mutex<Instant>>,
) {
    let clients = Arc::clone(clients);
    let activity = Arc::clone(activity);
    tokio::spawn(async move {
        if let Err(error) = serve_connection(stream, state, identity, &activity).await {
            warn!(%error, "agent connection stopped");
        }
        clients.fetch_sub(1, Ordering::AcqRel);
        *activity.lock().await = Instant::now();
    });
}

async fn idle(clients: &AtomicUsize, activity: &Mutex<Instant>) -> bool {
    clients.load(Ordering::Acquire) == 0 && activity.lock().await.elapsed() >= IDLE_EXIT
}

fn resolve_pi_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("REMAGIC_PI_BINARY") {
        return path.into();
    }
    // Production never borrows Paperweight/AppLoad's historical Node tree.
    // A missing or damaged packaged runtime must be visible as unavailable;
    // development overrides remain explicit through REMAGIC_PI_BINARY.
    PathBuf::from("/home/root/apps/remagic/runtime/pi/bin/pi")
}

fn peer_identity(stream: &UnixStream) -> Option<ClientIdentity> {
    let credentials = stream.peer_cred().ok()?;
    if credentials.uid() != unsafe { libc::geteuid() } {
        return None;
    }
    let pid = credentials.pid()?;
    let environment = fs::read(format!("/proc/{pid}/environ")).ok()?;
    let app_id = AppId::new(process_environment(&environment, "REMAGIC_APP_ID")?).ok()?;
    let generation = process_environment(&environment, "REMAGIC_APP_GENERATION")?
        .parse::<u64>()
        .ok()
        .filter(|value| *value != 0)?;
    let principal = process_environment(&environment, "REMAGIC_AGENT_PRINCIPAL")?;
    let unit = principal_unit(&app_id, &principal)?;
    let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    cgroup_contains_unit(&cgroup, &unit).then_some(ClientIdentity {
        app_id,
        generation,
        principal,
    })
}

fn cgroup_contains_unit(cgroup: &str, unit: &str) -> bool {
    cgroup.lines().any(|line| {
        line.splitn(3, ':')
            .nth(2)
            .is_some_and(|path| path.split('/').any(|component| component == unit))
    })
}

fn principal_unit(app_id: &AppId, principal: &str) -> Option<String> {
    match principal {
        "foreground" => Some(format!("remagic-app@{}.service", app_id.as_str())),
        "background" => Some(format!("remagic-background-{}.service", app_id.as_str())),
        _ => None,
    }
}

fn process_environment(environment: &[u8], key: &str) -> Option<String> {
    environment.split(|byte| *byte == 0).find_map(|entry| {
        let entry = std::str::from_utf8(entry).ok()?;
        let (candidate, value) = entry.split_once('=')?;
        (candidate == key).then(|| value.to_owned())
    })
}

async fn serve_connection(
    stream: UnixStream,
    state: AgentState,
    identity: ClientIdentity,
    activity: &Mutex<Instant>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut reader, mut writer) = stream.into_split();
    let (events, mut outgoing) = mpsc::channel::<AgentEvent>(64);
    let writer_task = tokio::spawn(async move {
        while let Some(event) = outgoing.recv().await {
            write_agent_frame(&mut writer, &event).await?;
        }
        Ok::<_, AgentFrameError>(())
    });
    let connection_turn = Arc::new(ConnectionTurn::new(None));
    let mut pinned_app: Option<AppId> = None;
    while let Some(message) = read_message(&mut reader).await? {
        *activity.lock().await = Instant::now();
        if !validate_message(&message, &state, &identity, &events).await {
            continue;
        }
        if pinned_app
            .as_ref()
            .is_some_and(|app_id| app_id != message.app_id())
        {
            send_error(
                &events,
                message.request_id(),
                message.app_id(),
                None,
                AgentErrorCode::InvalidRequest,
                "one connection may represent only one application",
                false,
            )
            .await;
            continue;
        }
        pinned_app.get_or_insert_with(|| message.app_id().clone());
        dispatch(message, &state, &identity, &events, &connection_turn).await;
    }
    cancel_connection_turn(&state, &connection_turn).await;
    drop(events);
    writer_task.await??;
    Ok(())
}

async fn read_message(
    reader: &mut tokio::net::unix::OwnedReadHalf,
) -> Result<Option<remagic_protocol::AgentClientMessage>, AgentFrameError> {
    match read_agent_frame(reader).await {
        Ok(message) => Ok(Some(message)),
        Err(AgentFrameError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

async fn validate_message(
    message: &remagic_protocol::AgentClientMessage,
    state: &AgentState,
    identity: &ClientIdentity,
    events: &mpsc::Sender<AgentEvent>,
) -> bool {
    if let Err(error) = message.validate() {
        send_error(
            events,
            message.request_id(),
            message.app_id(),
            None,
            AgentErrorCode::InvalidRequest,
            error.to_string(),
            false,
        )
        .await;
        return false;
    }
    if !state
        .authorize(identity, message.app_id(), message.client_token())
        .await
    {
        send_error(
            events,
            message.request_id(),
            message.app_id(),
            None,
            AgentErrorCode::InvalidRequest,
            "agent token is not bound to this managed application generation",
            false,
        )
        .await;
        return false;
    }
    true
}

fn activated_or_private_listener(path: &Path) -> std::io::Result<UnixListener> {
    if systemd_socket_available() {
        // SAFETY: systemd passes one owned listener at fd 3; consume it once.
        let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(SYSTEMD_LISTEN_FD) };
        listener.set_nonblocking(true)?;
        return UnixListener::from_std(listener);
    }
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.file_type().is_socket() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "refusing to replace non-socket agent path",
            ));
        }
        fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn systemd_socket_available() -> bool {
    std::env::var("LISTEN_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(std::process::id())
        && std::env::var("LISTEN_FDS").as_deref() == Ok("1")
}

#[cfg(test)]
mod tests;
