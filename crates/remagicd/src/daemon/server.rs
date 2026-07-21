use super::*;
use remagic_core::DomainState;
use remagic_protocol::{read_frame, write_frame, Request, Response};
use serde::Deserialize;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

pub(super) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(RUNTIME_ROOT)?;
    let listener = bind_private_socket(Path::new(remagic_protocol::DEFAULT_SOCKET))?;
    let app_listener = bind_private_socket(Path::new(APP_REQUEST_SOCKET))?;
    let (daemon, mut event_rx, power_thread) = create_daemon()?;

    daemon
        .initialize_system()
        .await
        .map_err(std::io::Error::other)?;
    spawn_control_server(listener, daemon.clone());
    spawn_app_request_server(app_listener, daemon.clone());
    spawn_signal_handler(daemon.clone());
    supervision::spawn(daemon.clone());
    info!(
        socket = remagic_protocol::DEFAULT_SOCKET,
        "Remagic manager ready"
    );

    let result = event_loop(daemon, &mut event_rx).await;
    drop(power_thread);
    result
}

fn bind_private_socket(path: &Path) -> std::io::Result<UnixListener> {
    let _ = fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

fn create_daemon() -> Result<DaemonParts, Box<dyn std::error::Error>> {
    let manifest_store =
        ManifestStore::new(utils::env_path("REMAGIC_MANIFEST_ROOT", MANIFEST_ROOT));
    let session_store = SessionStore::new(utils::env_path("REMAGIC_SESSION_ROOT", SESSION_ROOT));
    let manifests = manifest_store.load_all()?;
    let sessions = session_store.load_all()?;
    let (events, event_rx) = mpsc::channel(64);
    let launch_interrupt_epoch = Arc::new(AtomicU64::new(1));
    let (power_thread, power_control) =
        power_device::spawn(events.clone(), launch_interrupt_epoch.clone());
    let daemon = Arc::new(Daemon {
        state: RwLock::new(ManagerState::default()),
        manifests: RwLock::new(manifests),
        sessions: RwLock::new(sessions),
        runtime_generations: RwLock::new(BTreeMap::new()),
        runtime_foreground_fences: RwLock::new(BTreeMap::new()),
        runtime_exit_reports: RwLock::new(BTreeMap::new()),
        runtime_missing_observations: RwLock::new(BTreeMap::new()),
        session_store,
        manifest_store,
        controller: SystemController::new(),
        transition_lock: Mutex::new(()),
        events,
        power_control,
        next_generation: AtomicU64::new(1),
        next_foreground_epoch: AtomicU64::new(1),
        launch_interrupt_epoch,
        manager_repair_pending: AtomicBool::new(false),
        domain_recovery_pending: AtomicBool::new(false),
    });
    Ok((daemon, event_rx, power_thread))
}

type DaemonParts = (
    Arc<Daemon>,
    mpsc::Receiver<QueuedEvent>,
    std::thread::JoinHandle<()>,
);

impl Daemon {
    async fn initialize_system(&self) -> Result<(), String> {
        self.controller.ensure_system().await
    }
}

fn spawn_control_server(listener: UnixListener, daemon: Arc<Daemon>) {
    tokio::spawn(async move {
        accept_loop(listener, daemon, serve_control_client, "control").await;
    });
}

fn spawn_app_request_server(listener: UnixListener, daemon: Arc<Daemon>) {
    tokio::spawn(async move {
        accept_loop(listener, daemon, serve_app_request, "application request").await;
    });
}

async fn accept_loop<F, Fut>(
    listener: UnixListener,
    daemon: Arc<Daemon>,
    handler: F,
    label: &'static str,
) where
    F: Fn(UnixStream, Arc<Daemon>) -> Fut + Copy + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>>
        + Send
        + 'static,
{
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let daemon = daemon.clone();
                tokio::spawn(async move {
                    if let Err(error) = handler(stream, daemon).await {
                        warn!(%error, server = label, "client request failed");
                    }
                });
            }
            Err(error) => {
                error!(%error, server = label, "socket accept failed");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

fn spawn_signal_handler(daemon: Arc<Daemon>) {
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let mut exit_code = 0;
        let needs_restore = !matches!(daemon.state.read().await.domain, DomainState::System);
        if needs_restore && daemon.restore_system().await.is_err() {
            error!("system restore failed while stopping daemon");
            exit_code = 1;
        }
        std::process::exit(exit_code);
    });
}

async fn event_loop(
    daemon: Arc<Daemon>,
    events: &mut mpsc::Receiver<QueuedEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    while let Some(queued) = events.recv().await {
        let outcome = if queued.request_fence.is_cancelled() {
            Err("request was cancelled before execution".into())
        } else {
            daemon
                .handle_event(
                    queued.event,
                    queued.interrupt_epoch,
                    queued.request_fence.clone(),
                )
                .await
        };
        let recovery = recover_after_failure(&daemon, &outcome).await;
        if let Some(reply) = queued.reply {
            let _ = reply.send(outcome);
        }
        if let Err(error) = recovery {
            return Err(std::io::Error::other(error).into());
        }
    }
    Ok(())
}

async fn recover_after_failure(
    daemon: &Daemon,
    outcome: &Result<(), String>,
) -> Result<(), String> {
    let Err(error) = outcome else { return Ok(()) };
    error!(%error, "transition failed");
    let domain = daemon.state.read().await.domain.clone();
    match domain {
        DomainState::System | DomainState::Foreground(_) => Ok(()),
        DomainState::Manager => daemon.ensure_manager_or_restore().await,
        DomainState::EnteringManaged
        | DomainState::Launching(_)
        | DomainState::Parking(_)
        | DomainState::RestoringSystem
        | DomainState::Sleeping
        | DomainState::Recovering => daemon.restore_system().await,
    }
}

async fn serve_control_client(
    mut stream: UnixStream,
    daemon: Arc<Daemon>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        write_frame(
            &mut stream,
            &Response::Error {
                message: "permission denied".into(),
            },
        )
        .await?;
        return Ok(());
    }
    let request: Request = read_frame(&mut stream).await?;
    write_frame(&mut stream, &daemon.request(request).await).await?;
    Ok(())
}

#[derive(Deserialize)]
struct LegacyAppRequest {
    #[serde(default)]
    version: u8,
    #[serde(default)]
    request_id: String,
    command: String,
    app: String,
    #[serde(default)]
    open_path: Option<PathBuf>,
}

async fn serve_app_request(
    mut stream: UnixStream,
    daemon: Arc<Daemon>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        stream
            .write_all(b"{\"ok\":false,\"error\":\"permission denied\"}\n")
            .await?;
        return Ok(());
    }
    let mut line = String::new();
    let count = BufReader::new(&mut stream).read_line(&mut line).await?;
    let response = parse_app_request(count, &line, &daemon).await;
    let reply = response_to_json(response);
    stream.write_all(&serde_json::to_vec(&reply)?).await?;
    stream.write_all(b"\n").await?;
    Ok(())
}

async fn parse_app_request(count: usize, line: &str, daemon: &Daemon) -> Response {
    if count == 0 || !line.ends_with('\n') || line.len() > 64 * 1024 {
        return Response::Error {
            message: "incomplete application request".into(),
        };
    }
    match serde_json::from_str::<LegacyAppRequest>(line) {
        Ok(request)
            if request.version == 1
                && !request.request_id.is_empty()
                && request.command == "open_app" =>
        {
            match AppId::new(request.app) {
                Ok(app_id) => {
                    daemon
                        .enqueue(Event::Launch(app_id, request.open_path))
                        .await
                }
                Err(error) => Response::Error {
                    message: error.to_string(),
                },
            }
        }
        Ok(_) => Response::Error {
            message: "unsupported application request".into(),
        },
        Err(error) => Response::Error {
            message: format!("invalid application request: {error}"),
        },
    }
}

fn response_to_json(response: Response) -> serde_json::Value {
    match response {
        Response::Ok => serde_json::json!({"ok": true, "status": "accepted"}),
        Response::Error { message } => serde_json::json!({"ok": false, "error": message}),
        _ => serde_json::json!({"ok": false, "error": "unexpected manager reply"}),
    }
}
