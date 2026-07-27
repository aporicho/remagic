use super::*;
use crate::display_host;
use remagic_core::DomainState;
use remagic_package::PackageManager;
use remagic_protocol::{
    read_frame, write_frame, ControlErrorCode, ControlIntent, ControlReply, ControlRequest,
    Request, Response,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

mod app_requests;

const CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(5);
const FAST_CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const LIST_CONTROL_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(60);
const LONG_CONTROL_TIMEOUT: Duration = Duration::from_secs(180);
const PACKAGE_CONTROL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SHUTDOWN_RESTORE_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    fs::create_dir_all(RUNTIME_ROOT)?;
    let listener = bind_private_socket(Path::new(remagic_protocol::DEFAULT_SOCKET))?;
    let app_listener = bind_private_socket(Path::new(APP_REQUEST_SOCKET))?;
    let activity_listener = bind_private_socket(Path::new(ACTIVITY_SOCKET))?;
    let (daemon, mut event_rx, power_thread) = create_daemon()?;

    daemon
        .initialize_system()
        .await
        .map_err(std::io::Error::other)?;
    daemon.start_declared_background_services().await;
    spawn_control_server(listener, daemon.clone());
    spawn_app_request_server(app_listener, daemon.clone());
    spawn_activity_server(activity_listener, daemon.clone());
    spawn_signal_handler(daemon.clone());
    info!(
        socket = remagic_protocol::DEFAULT_SOCKET,
        "ReMagic manager ready"
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
    let package_manager = PackageManager::from_environment();
    package_manager.recover_all()?;
    fs::create_dir_all(&package_manager.paths().books_root)?;
    let manifest_store =
        ManifestStore::new(utils::env_path("REMAGIC_MANIFEST_ROOT", MANIFEST_ROOT));
    let session_store = SessionStore::new(utils::env_path("REMAGIC_SESSION_ROOT", SESSION_ROOT));
    let manifests = manifest_store.load_all()?;
    let sessions = session_store.load_all()?;
    let (events, event_rx) = mpsc::channel(64);
    let launch_interrupt_epoch = Arc::new(AtomicU64::new(1));
    let cover_closed = Arc::new(AtomicBool::new(false));
    let power = Arc::new(crate::power_manager::PowerManager::load());
    let backlight = Arc::new(crate::backlight::BacklightManager::load());
    let (power_thread, power_control) = power_device::spawn(
        events.clone(),
        launch_interrupt_epoch.clone(),
        cover_closed.clone(),
    );
    let daemon = Arc::new(Daemon {
        state: RwLock::new(ManagerState::default()),
        manifests: RwLock::new(manifests),
        sessions: RwLock::new(sessions),
        runtime_generations: RwLock::new(BTreeMap::new()),
        runtime_background_execution: RwLock::new(BTreeMap::new()),
        runtime_foreground_fences: RwLock::new(BTreeMap::new()),
        runtime_input_modes: RwLock::new(BTreeMap::new()),
        runtime_exit_reports: RwLock::new(BTreeMap::new()),
        runtime_missing_observations: RwLock::new(BTreeMap::new()),
        session_store,
        manifest_store,
        controller: SystemController::new(),
        power: power.clone(),
        backlight,
        transition_lock: Mutex::new(()),
        events,
        power_control,
        next_generation: AtomicU64::new(1),
        next_foreground_epoch: AtomicU64::new(1),
        next_sleep_epoch: AtomicU64::new(1),
        sleep_transaction: sleep::SleepTransaction::default(),
        launch_interrupt_epoch,
        cover_closed,
        cover_resume_app: RwLock::new(None),
        #[cfg(test)]
        manager_repair_pending: AtomicBool::new(false),
        domain_recovery_pending: AtomicBool::new(false),
    });
    power.spawn(daemon.events.clone(), daemon.launch_interrupt_epoch.clone());
    Ok((daemon, event_rx, power_thread))
}

type DaemonParts = (
    Arc<Daemon>,
    mpsc::Receiver<QueuedEvent>,
    std::thread::JoinHandle<()>,
);

impl Daemon {
    async fn initialize_system(&self) -> Result<(), String> {
        self.controller.ensure_system().await?;
        if let Err(error) = utils::set_foreground_marker(None) {
            warn!(%error, "could not clear stale foreground marker during startup recovery");
        }
        Ok(())
    }
}

fn spawn_control_server(listener: UnixListener, daemon: Arc<Daemon>) {
    tokio::spawn(async move {
        accept_loop(listener, daemon, serve_control_client, "control").await;
    });
}

fn spawn_app_request_server(listener: UnixListener, daemon: Arc<Daemon>) {
    tokio::spawn(async move {
        accept_loop(listener, daemon, app_requests::serve, "application request").await;
    });
}

fn spawn_activity_server(listener: UnixListener, daemon: Arc<Daemon>) {
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    if !display_activity_peer(&stream) {
                        warn!("rejected activity source outside remagic-display-host.service");
                        continue;
                    }
                    let daemon = daemon.clone();
                    tokio::spawn(async move {
                        let mut stream = stream;
                        let mut bytes = [0_u8; 64];
                        loop {
                            match stream.read(&mut bytes).await {
                                Ok(0) => break,
                                Ok(_) => daemon.power.note_activity().await,
                                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                                Err(_) => break,
                            }
                        }
                    });
                }
                Err(error) => {
                    error!(%error, "activity socket accept failed");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    });
}

fn display_activity_peer(stream: &UnixStream) -> bool {
    let Ok(credentials) = stream.peer_cred() else {
        return false;
    };
    if credentials.uid() != unsafe { libc::geteuid() } {
        return false;
    }
    let Some(pid) = credentials.pid() else {
        return false;
    };
    fs::read_to_string(format!("/proc/{pid}/cgroup")).is_ok_and(|value| {
        value
            .split('/')
            .any(|part| part.trim() == "remagic-display-host.service")
    })
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
        let exit_code =
            match tokio::time::timeout(SHUTDOWN_RESTORE_TIMEOUT, shutdown_restore(&daemon)).await {
                Ok(Ok(())) => 0,
                Ok(Err(error)) => {
                    error!(%error, "system restore failed while stopping daemon");
                    1
                }
                Err(_) => {
                    error!(
                        timeout_ms = SHUTDOWN_RESTORE_TIMEOUT.as_millis(),
                        "system restore timed out while stopping daemon"
                    );
                    1
                }
            };
        std::process::exit(exit_code);
    });
}

async fn shutdown_restore(daemon: &Daemon) -> Result<(), String> {
    if let Err(error) = utils::set_foreground_marker(None) {
        warn!(%error, "could not clear foreground marker while stopping daemon");
    }
    if let Err(error) = daemon.sleep_transaction.reset() {
        warn!(%error, "could not reset sleep transaction while stopping daemon");
    }
    daemon.backlight.restore_desired();
    daemon.controller.ensure_system().await
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
        DomainState::Sleeping
            if should_retain_sleeping_failure(
                error,
                retained_sleep_lock_is_healthy(daemon).await,
            ) =>
        {
            Ok(())
        }
        DomainState::EnteringManaged
        | DomainState::Launching(_)
        | DomainState::Parking(_)
        | DomainState::RestoringSystem
        | DomainState::Sleeping
        | DomainState::Recovering => daemon.restore_system().await,
    }
}

fn should_retain_sleeping_failure(error: &str, healthy_lock: bool) -> bool {
    sleep::is_retained_lock_error(error) && healthy_lock
}

async fn retained_sleep_lock_is_healthy(daemon: &Daemon) -> bool {
    let transaction = daemon.sleep_transaction.snapshot();
    if transaction.epoch == 0 || transaction.phase != sleep::SleepPhase::Locked {
        return false;
    }
    display_host::status()
        .await
        .is_ok_and(|display| display.lock_committed && display.lock_epoch == transaction.epoch)
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
    let value: serde_json::Value =
        tokio::time::timeout(CONTROL_READ_TIMEOUT, read_frame(&mut stream))
            .await
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "control frame read timed out")
            })??;
    if value.get("protocol").is_some() {
        let request: ControlRequest = serde_json::from_value(value)?;
        let timeout = control_v2_timeout(&request.body);
        let request_id = request.request_id.clone();
        let response = match tokio::time::timeout(timeout, daemon.control_v2(request)).await {
            Ok(response) => response,
            Err(_) => control_v2_timeout_response(request_id, timeout),
        };
        write_frame(&mut stream, &response).await?;
    } else {
        let request: Request = serde_json::from_value(value)?;
        let timeout = legacy_control_timeout(&request);
        let response = match tokio::time::timeout(timeout, daemon.request(request)).await {
            Ok(response) => response,
            Err(_) => Response::Error {
                message: format!(
                    "manager control request timed out after {} ms",
                    timeout.as_millis()
                ),
            },
        };
        write_frame(&mut stream, &response).await?;
    }
    Ok(())
}

fn control_v2_timeout(intent: &ControlIntent) -> Duration {
    match intent {
        ControlIntent::Snapshot
        | ControlIntent::PowerSnapshot
        | ControlIntent::BacklightSnapshot
        | ControlIntent::SetIdleSuspend { .. }
        | ControlIntent::SetBacklight { .. }
        | ControlIntent::Subscribe { .. } => FAST_CONTROL_TIMEOUT,
        ControlIntent::Preflight { .. } => LIST_CONTROL_TIMEOUT,
        ControlIntent::Install { .. }
        | ControlIntent::Upgrade { .. }
        | ControlIntent::Rollback { .. }
        | ControlIntent::Uninstall { .. } => PACKAGE_CONTROL_TIMEOUT,
        ControlIntent::LegacyPackage { .. } => LONG_CONTROL_TIMEOUT,
        ControlIntent::ReloadManifests
        | ControlIntent::ShowHome
        | ControlIntent::ReturnStock
        | ControlIntent::Sleep
        | ControlIntent::Wake
        | ControlIntent::Launch { .. }
        | ControlIntent::OpenPath { .. }
        | ControlIntent::ParkCurrent
        | ControlIntent::Close { .. } => DEFAULT_CONTROL_TIMEOUT,
    }
}

fn legacy_control_timeout(request: &Request) -> Duration {
    match request {
        Request::Status
        | Request::PowerStatus
        | Request::BacklightStatus
        | Request::SetIdleSuspend { .. }
        | Request::SetBacklight { .. }
        | Request::Notify { .. } => FAST_CONTROL_TIMEOUT,
        Request::ListApps => LIST_CONTROL_TIMEOUT,
        Request::Package { .. } | Request::Sync { .. } => LONG_CONTROL_TIMEOUT,
        Request::ReloadManifests
        | Request::OpenManager
        | Request::ReturnSystem
        | Request::Sleep { .. }
        | Request::Wake { .. }
        | Request::Launch { .. }
        | Request::ParkCurrent
        | Request::Close { .. }
        | Request::RuntimeExited { .. }
        | Request::Ready { .. }
        | Request::Parked { .. } => DEFAULT_CONTROL_TIMEOUT,
    }
}

fn control_v2_timeout_response(
    request_id: String,
    timeout: Duration,
) -> remagic_protocol::ControlResponse {
    remagic_protocol::Envelope::new(
        request_id,
        ControlReply::Error {
            code: ControlErrorCode::Timeout,
            message: format!(
                "manager control request timed out after {} ms",
                timeout.as_millis()
            ),
            state_revision: None,
        },
    )
}

#[cfg(test)]
mod recovery_tests {
    use super::{
        control_v2_timeout, control_v2_timeout_response, legacy_control_timeout,
        should_retain_sleeping_failure, FAST_CONTROL_TIMEOUT, LONG_CONTROL_TIMEOUT,
        PACKAGE_CONTROL_TIMEOUT,
    };
    use crate::daemon::sleep;
    use remagic_protocol::{ControlErrorCode, ControlIntent, ControlReply, Request};

    #[test]
    fn healthy_notification_failure_never_selects_stock_restore() {
        let failure = sleep::retained_lock_error("Home datagram failed");
        assert!(should_retain_sleeping_failure(&failure, true));
        assert!(!should_retain_sleeping_failure(&failure, false));
        assert!(!should_retain_sleeping_failure(
            "Home datagram failed",
            true
        ));
    }

    #[test]
    fn status_controls_have_short_deadlines() {
        assert_eq!(
            legacy_control_timeout(&Request::Status),
            FAST_CONTROL_TIMEOUT
        );
        assert_eq!(
            control_v2_timeout(&ControlIntent::Snapshot),
            FAST_CONTROL_TIMEOUT
        );
        assert_eq!(
            control_v2_timeout(&ControlIntent::Install {
                bundle: "/tmp/app.remagic".into()
            }),
            PACKAGE_CONTROL_TIMEOUT
        );
        assert_eq!(
            legacy_control_timeout(&Request::Sync {
                requester: remagic_core::AppId::new("upload").unwrap(),
                provider: remagic_core::AppId::new("koreader").unwrap(),
                action: remagic_protocol::SyncAction::Prepare,
            }),
            LONG_CONTROL_TIMEOUT
        );
    }

    #[test]
    fn v2_timeout_uses_explicit_error_code() {
        let response = control_v2_timeout_response("request-1".into(), FAST_CONTROL_TIMEOUT);
        match response.body {
            ControlReply::Error { code, .. } => assert_eq!(code, ControlErrorCode::Timeout),
            other => panic!("unexpected timeout response: {other:?}"),
        }
    }
}
