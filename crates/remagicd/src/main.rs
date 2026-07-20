mod power_device;
mod runtime;
mod system;

use remagic_core::{
    AppId, AppSession, DomainState, ManagerState, ManifestStore, SessionStatus, SessionStore,
    Transition,
};
use remagic_protocol::{read_frame, write_frame, AppView, PackageOperation, Request, Response};
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use system::SystemController;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, Mutex, RwLock};
use tracing::{error, info, warn};

const MANIFEST_ROOT: &str = "/home/root/.local/share/remagic/apps.d";
const SESSION_ROOT: &str = "/home/root/.local/state/remagic/sessions";
const RUNTIME_ROOT: &str = "/run/remagic";
const HOME_UNIT: &str = "remagic-runtime.service";
const FOREGROUND_MARKER: &str = "/run/remagic/foreground-app";

#[derive(Debug)]
enum Event {
    SinglePower,
    TriplePower,
    LongPower,
    Launch(AppId, Option<PathBuf>),
    OpenManager,
    ReturnSystem,
    Sleep,
    Close(AppId, bool),
    RuntimeExited {
        app_id: AppId,
        generation: u64,
        exit_code: i32,
        crashed: bool,
    },
    AppReady(AppId),
    AppParked(AppSession),
    Package(PackageOperation),
    ReloadManifests,
}

struct QueuedEvent {
    event: Event,
    reply: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
}

struct Daemon {
    state: RwLock<ManagerState>,
    manifests: RwLock<BTreeMap<AppId, remagic_core::AppManifest>>,
    sessions: RwLock<BTreeMap<AppId, AppSession>>,
    runtime_generations: RwLock<BTreeMap<AppId, u64>>,
    session_store: SessionStore,
    manifest_store: ManifestStore,
    controller: SystemController,
    transition_lock: Mutex<()>,
    events: mpsc::Sender<QueuedEvent>,
    power_control: std::sync::mpsc::Sender<power_device::Control>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "remagicd=info".into()),
        )
        .init();

    fs::create_dir_all(RUNTIME_ROOT)?;
    let socket = Path::new(remagic_protocol::DEFAULT_SOCKET);
    let _ = fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;

    let manifest_store = ManifestStore::new(env_path("REMAGIC_MANIFEST_ROOT", MANIFEST_ROOT));
    let session_store = SessionStore::new(env_path("REMAGIC_SESSION_ROOT", SESSION_ROOT));
    let manifests = manifest_store.load_all()?;
    let sessions = session_store.load_all()?;
    let (event_tx, mut event_rx) = mpsc::channel(64);
    let (power_tx, power_control) = power_device::spawn(event_tx.clone());
    let daemon = Arc::new(Daemon {
        state: RwLock::new(ManagerState::default()),
        manifests: RwLock::new(manifests),
        sessions: RwLock::new(sessions),
        runtime_generations: RwLock::new(BTreeMap::new()),
        session_store,
        manifest_store,
        controller: SystemController::new(),
        transition_lock: Mutex::new(()),
        events: event_tx,
        power_control,
    });

    if let Err(error) = daemon.controller.ensure_system().await {
        warn!(%error, "initial system-shell check failed");
    }
    info!(socket = %socket.display(), "Remagic manager ready");

    let accept_daemon = daemon.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let daemon = accept_daemon.clone();
                    tokio::spawn(async move {
                        if let Err(error) = serve_client(stream, daemon).await {
                            warn!(%error, "client request failed");
                        }
                    });
                }
                Err(error) => {
                    error!(%error, "control socket accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
    });

    let signal_daemon = daemon.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let mut exit_code = 0;
        if !matches!(signal_daemon.state.read().await.domain, DomainState::System) {
            if let Err(error) = signal_daemon.restore_system().await {
                error!(%error, "system restore failed while stopping daemon");
                exit_code = 1;
            }
        }
        std::process::exit(exit_code);
    });

    while let Some(queued) = event_rx.recv().await {
        let outcome = daemon.handle_event(queued.event).await;
        let mut fatal_recovery_error = None;
        if let Err(error) = &outcome {
            error!(%error, "transition failed");
            let domain = daemon.state.read().await.domain.clone();
            match domain {
                // An application-level error (missing file, rejected path,
                // transient launch failure, and so on) must not tear down the
                // complete managed display domain.  Keep the current app, or
                // reveal the manager after launch() has rolled back to it.
                DomainState::System | DomainState::Foreground(_) => {}
                DomainState::Manager => {
                    if let Err(runtime_error) = runtime::show_manager().await {
                        warn!(%runtime_error, "could not restore manager after request failure");
                    }
                }
                // Failures in an ownership transition can leave neither UI
                // authoritative.  Only those states warrant the stock-shell
                // recovery path.
                DomainState::EnteringManaged
                | DomainState::Launching(_)
                | DomainState::Parking(_)
                | DomainState::RestoringSystem
                | DomainState::Sleeping
                | DomainState::Recovering => {
                    if let Err(recovery_error) = daemon.restore_system().await {
                        error!(%recovery_error, "stock-shell recovery failed");
                        fatal_recovery_error = Some(recovery_error);
                    }
                }
            }
        }
        if let Some(reply) = queued.reply {
            let _ = reply.send(outcome);
        }
        if let Some(error) = fatal_recovery_error {
            return Err(std::io::Error::other(error).into());
        }
    }
    drop(power_tx);
    Ok(())
}

fn env_path(key: &str, fallback: &str) -> PathBuf {
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback.into())
}

async fn serve_client(
    mut stream: UnixStream,
    daemon: Arc<Daemon>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let credentials = stream.peer_cred()?;
    let expected_uid = unsafe { libc::geteuid() };
    if credentials.uid() != expected_uid {
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
    let response = daemon.request(request).await;
    write_frame(&mut stream, &response).await?;
    Ok(())
}

impl Daemon {
    async fn request(&self, request: Request) -> Response {
        match request {
            Request::Status => {
                let state = self.state.read().await;
                Response::Status {
                    domain: state.domain.clone(),
                    last_app: state.last_app.clone(),
                    sequence: state.sequence,
                }
            }
            Request::ListApps => Response::Apps {
                apps: self.app_views().await,
            },
            Request::ReloadManifests => self.enqueue(Event::ReloadManifests).await,
            Request::OpenManager => self.enqueue(Event::OpenManager).await,
            Request::ReturnSystem => self.enqueue(Event::ReturnSystem).await,
            Request::Sleep => self.enqueue(Event::Sleep).await,
            Request::Launch { app_id, open_path } => {
                self.enqueue(Event::Launch(app_id, open_path)).await
            }
            Request::ParkCurrent => self.enqueue(Event::SinglePower).await,
            Request::Close { app_id, complete } => {
                self.enqueue(Event::Close(app_id, complete)).await
            }
            Request::RuntimeExited {
                app_id,
                generation,
                exit_code,
                crashed,
            } => {
                self.enqueue(Event::RuntimeExited {
                    app_id,
                    generation,
                    exit_code,
                    crashed,
                })
                .await
            }
            Request::Ready { app_id } => self.enqueue(Event::AppReady(app_id)).await,
            Request::Parked {
                app_id,
                title,
                subtitle,
                resume_payload,
            } => {
                let session = AppSession {
                    schema: 1,
                    app_id,
                    status: SessionStatus::Parked,
                    title,
                    subtitle,
                    resume_payload,
                    updated_at: unix_now(),
                    last_error: None,
                };
                self.enqueue(Event::AppParked(session)).await
            }
            Request::Notify {
                app_id,
                title,
                body,
            } => {
                info!(%app_id, %title, %body, "application notification queued");
                Response::Ok
            }
            Request::Package { operation } => self.enqueue(Event::Package(operation)).await,
        }
    }

    async fn enqueue(&self, event: Event) -> Response {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if self
            .events
            .send(QueuedEvent {
                event,
                reply: Some(reply_tx),
            })
            .await
            .is_err()
        {
            return Response::Error {
                message: "manager event loop is unavailable".into(),
            };
        }
        match tokio::time::timeout(std::time::Duration::from_secs(20), reply_rx).await {
            Ok(Ok(Ok(()))) => Response::Ok,
            Ok(Ok(Err(message))) => Response::Error { message },
            Ok(Err(_)) => Response::Error {
                message: "manager event loop dropped the request".into(),
            },
            Err(_) => Response::Error {
                message: "manager request timed out".into(),
            },
        }
    }

    async fn app_views(&self) -> Vec<AppView> {
        let state = self.state.read().await.clone();
        let manifests: Vec<_> = self.manifests.read().await.values().cloned().collect();
        let sessions = self.sessions.read().await.clone();
        let mut views = Vec::with_capacity(manifests.len());
        for manifest in manifests {
            let background_active = match &manifest.background_unit {
                Some(unit) => self.controller.is_active(unit).await,
                None => false,
            };
            views.push(AppView {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                description: manifest.description.clone(),
                installed: manifest.exec.exists() && manifest.working_dir.exists(),
                foreground: matches!(&state.domain, DomainState::Foreground(id) if id == &manifest.id),
                background_service: manifest.background_unit.clone(),
                background_active,
                session: sessions.get(&manifest.id).cloned(),
                package: manifest.package.clone(),
            });
        }
        views
    }

    async fn handle_event(&self, event: Event) -> Result<(), String> {
        match event {
            Event::SinglePower => self.single_power().await,
            Event::TriplePower => self.triple_power().await,
            Event::LongPower => {
                // Never steal the vendor shell's long-press power menu. The
                // manager exposes an explicit sleep button instead.
                info!("long power press left to the active foreground domain");
                Ok(())
            }
            Event::Launch(id, path) => self.launch(id, path).await,
            Event::OpenManager => self.open_manager().await,
            Event::ReturnSystem => self.restore_system().await,
            Event::Sleep => self.sleep().await,
            Event::Close(id, complete) => self.close(id, complete).await,
            Event::RuntimeExited {
                app_id,
                generation,
                exit_code,
                crashed,
            } => {
                self.record_runtime_exit(app_id, generation, exit_code, crashed)
                    .await
            }
            Event::AppReady(id) => {
                let mut state = self.state.write().await;
                if !matches!(&state.domain, DomainState::Launching(expected) if expected == &id) {
                    warn!(%id, domain = ?state.domain, "ignored stale application-ready event");
                    return Ok(());
                }
                state
                    .apply(Transition::AppReady(id.clone()))
                    .map_err(|e| e.to_string())?;
                if let DomainState::Foreground(id) = &state.domain {
                    set_foreground_marker(Some(id))?;
                }
                Ok(())
            }
            Event::AppParked(session) => self.record_parked(session).await,
            Event::Package(operation) => self.package(operation).await,
            Event::ReloadManifests => {
                let manifests = self
                    .manifest_store
                    .load_all()
                    .map_err(|error| error.to_string())?;
                *self.manifests.write().await = manifests;
                if matches!(self.state.read().await.domain, DomainState::Manager) {
                    if let Err(error) = self.restart_runtime_and_wait().await {
                        warn!(%error, "runtime reload failed; restoring stock interface");
                        return self.restore_system().await;
                    }
                }
                Ok(())
            }
        }
    }

    async fn single_power(&self) -> Result<(), String> {
        let domain = self.state.read().await.domain.clone();
        match domain {
            DomainState::System => Ok(()),
            DomainState::Manager => {
                let last = self.state.read().await.last_app.clone();
                if let Some(app) = last {
                    self.launch(app, None).await
                } else {
                    Ok(())
                }
            }
            DomainState::Foreground(app) => self.park(app, false, true).await,
            _ => Ok(()),
        }
    }

    async fn triple_power(&self) -> Result<(), String> {
        let domain = self.state.read().await.domain.clone();
        match domain {
            DomainState::System => self.enter_manager().await,
            DomainState::Foreground(app) => {
                self.park(app, true, false).await?;
                self.restore_system().await
            }
            DomainState::Manager => self.restore_system().await,
            _ => Ok(()),
        }
    }

    async fn enter_manager(&self) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        {
            let mut state = self.state.write().await;
            state
                .apply(Transition::TriplePower)
                .map_err(|e| e.to_string())?;
        }
        self.set_power_grab(true).await?;
        self.controller.enter_managed().await?;
        set_foreground_marker(None)?;
        self.controller.start(HOME_UNIT).await?;
        wait_runtime_ready().await?;
        self.state
            .write()
            .await
            .apply(Transition::ManagedReady)
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    async fn open_manager(&self) -> Result<(), String> {
        let domain = self.state.read().await.domain.clone();
        match domain {
            DomainState::System => self.enter_manager().await,
            DomainState::Foreground(app) => self.park(app, false, true).await,
            DomainState::Manager => Ok(()),
            _ => Ok(()),
        }
    }

    async fn launch(&self, id: AppId, open_path: Option<PathBuf>) -> Result<(), String> {
        let domain = self.state.read().await.domain.clone();
        match domain {
            DomainState::Foreground(current) if current == id => return Ok(()),
            DomainState::Foreground(current) => self.park(current, false, false).await?,
            DomainState::Manager => {}
            _ => {
                return Err(
                    "applications can only be launched from the manager or foreground app".into(),
                )
            }
        }
        let _guard = self.transition_lock.lock().await;
        if !matches!(self.state.read().await.domain, DomainState::Manager) {
            return Err("manager did not become ready for application launch".into());
        }
        let manifest = self
            .manifests
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("unknown application {id}"))?;
        if !manifest.exec.exists() {
            return Err(format!(
                "application executable is missing: {}",
                manifest.exec.display()
            ));
        }
        let open_path = match open_path {
            Some(path) => Some(
                manifest
                    .validate_open_path(&path)
                    .map_err(|e| e.to_string())?,
            ),
            None => None,
        };
        let resume_payload = self
            .sessions
            .read()
            .await
            .get(&id)
            .and_then(|session| session.resume_payload.clone());
        let launch_dir = Path::new(RUNTIME_ROOT).join("launch");
        fs::create_dir_all(&launch_dir).map_err(|e| e.to_string())?;
        let launch = serde_json::json!({
            "open_path": open_path,
            "resume_payload": resume_payload,
        });
        atomic_write_json(&launch_dir.join(format!("{}.json", id.as_str())), &launch)?;
        if let Some(unit) = &manifest.background_unit {
            if !self.controller.is_active(unit).await {
                self.controller.start(unit).await?;
            }
        }
        self.state
            .write()
            .await
            .apply(Transition::Launch(id.clone()))
            .map_err(|e| e.to_string())?;
        let runtime_launch = match runtime::open_app(&id, open_path.as_deref()).await {
            Ok(launch) => launch,
            Err(error) => {
                if let Err(close_error) = runtime::close_app(&id).await {
                    warn!(%id, %close_error, "failed launch could not be cleaned up by runtime");
                }
                self.runtime_generations.write().await.remove(&id);
                self.state
                    .write()
                    .await
                    .apply(Transition::AppExited(id.clone()))
                    .map_err(|state_error| state_error.to_string())?;
                return Err(format!("{error}; {} launch was rolled back", id.as_str()));
            }
        };
        self.runtime_generations
            .write()
            .await
            .insert(id.clone(), runtime_launch.generation);
        self.state
            .write()
            .await
            .apply(Transition::AppReady(id.clone()))
            .map_err(|e| e.to_string())?;
        set_foreground_marker(Some(&id))
    }

    async fn park(
        &self,
        id: AppId,
        returning_system: bool,
        show_manager: bool,
    ) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        self.state
            .write()
            .await
            .apply(if returning_system {
                Transition::TriplePower
            } else {
                Transition::SinglePower
            })
            .map_err(|e| e.to_string())?;
        set_foreground_marker(None)?;
        if show_manager {
            runtime::show_manager().await?;
        }
        let manifest = self.manifests.read().await.get(&id).cloned();
        let session = AppSession {
            schema: 1,
            app_id: id.clone(),
            status: SessionStatus::Parked,
            title: manifest
                .as_ref()
                .map(|m| m.name.clone())
                .unwrap_or_else(|| id.to_string()),
            subtitle: "已暂停，可继续".into(),
            resume_payload: self
                .sessions
                .read()
                .await
                .get(&id)
                .and_then(|value| value.resume_payload.clone())
                .or_else(|| Some(serde_json::json!({ "parked": true }))),
            updated_at: unix_now(),
            last_error: None,
        };
        self.save_session(session).await?;
        set_foreground_marker(None)?;
        let mut state = self.state.write().await;
        match &state.domain {
            DomainState::Parking(expected) if expected == &id => {
                state
                    .apply(Transition::AppParked(id))
                    .map_err(|e| e.to_string())?;
            }
            // The runner may report its parked session while systemd is still
            // stopping the unit. In that race record_parked already completed
            // the transition and started the manager.
            DomainState::Manager => {}
            current => {
                return Err(format!(
                    "application stopped in unexpected state: {current:?}"
                ))
            }
        };
        drop(state);
        Ok(())
    }

    async fn record_parked(&self, session: AppSession) -> Result<(), String> {
        let id = session.app_id.clone();
        // Legacy application callbacks do not carry a launch generation.
        // Accept them only while this exact app is already in the serialized
        // Parking transition; otherwise a delayed callback could tear down a
        // newly launched instance of the same app.
        if !matches!(
            &self.state.read().await.domain,
            DomainState::Parking(expected) if expected == &id
        ) {
            warn!(%id, "ignored stale application-parked event");
            return Ok(());
        }
        self.save_session(session).await?;
        set_foreground_marker(None)?;
        let mut state = self.state.write().await;
        match &state.domain {
            DomainState::Parking(expected) if expected == &id => {
                state
                    .apply(Transition::AppParked(id))
                    .map_err(|e| e.to_string())?;
            }
            _ => return Ok(()),
        }
        Ok(())
    }

    async fn save_session(&self, session: AppSession) -> Result<(), String> {
        self.session_store
            .save(&session)
            .map_err(|e| e.to_string())?;
        self.sessions
            .write()
            .await
            .insert(session.app_id.clone(), session);
        Ok(())
    }

    async fn close(&self, id: AppId, complete: bool) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        let domain = self.state.read().await.domain.clone();
        if matches!(&domain, DomainState::Foreground(current) if current == &id) {
            set_foreground_marker(None)?;
        }
        runtime::close_app(&id).await?;
        self.runtime_generations.write().await.remove(&id);
        if matches!(&domain, DomainState::Foreground(current) if current == &id) {
            self.state
                .write()
                .await
                .apply(Transition::AppExited(id.clone()))
                .map_err(|error| error.to_string())?;
        }
        self.session_store.remove(&id).map_err(|e| e.to_string())?;
        self.sessions.write().await.remove(&id);
        if complete {
            if let Some(unit) = self
                .manifests
                .read()
                .await
                .get(&id)
                .and_then(|manifest| manifest.background_unit.clone())
            {
                self.controller.stop(&unit).await?;
            }
        }
        Ok(())
    }

    async fn record_runtime_exit(
        &self,
        id: AppId,
        generation: u64,
        exit_code: i32,
        reported_crash: bool,
    ) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        let crashed = runtime_exit_is_crash(exit_code, reported_crash);
        {
            let mut generations = self.runtime_generations.write().await;
            if !runtime_generation_matches(&generations, &id, generation) {
                warn!(
                    %id,
                    generation,
                    expected_generation = ?generations.get(&id),
                    exit_code,
                    crashed,
                    "ignored stale runtime-exit notification"
                );
                return Ok(());
            }
            generations.remove(&id);
        }

        let domain = self.state.read().await.domain.clone();
        if matches!(&domain, DomainState::Foreground(current) if current == &id) {
            set_foreground_marker(None)?;
        }
        let owns_domain = matches!(
            &domain,
            DomainState::Launching(current)
                | DomainState::Foreground(current)
                | DomainState::Parking(current)
                if current == &id
        );
        if owns_domain {
            self.state
                .write()
                .await
                .apply(if crashed {
                    Transition::AppCrashed(id.clone())
                } else {
                    Transition::AppExited(id.clone())
                })
                .map_err(|error| error.to_string())?;
        }

        if crashed {
            let existing = self.sessions.read().await.get(&id).cloned();
            let title = self
                .manifests
                .read()
                .await
                .get(&id)
                .map(|manifest| manifest.name.clone())
                .unwrap_or_else(|| id.to_string());
            self.save_session(AppSession {
                schema: 1,
                app_id: id.clone(),
                status: SessionStatus::Crashed,
                title,
                subtitle: "应用异常退出".into(),
                resume_payload: existing.and_then(|session| session.resume_payload),
                updated_at: unix_now(),
                last_error: Some(format!("process exited with code {exit_code}")),
            })
            .await?;
            warn!(%id, generation, exit_code, "managed application crashed");
        } else {
            self.session_store.remove(&id).map_err(|e| e.to_string())?;
            self.sessions.write().await.remove(&id);
            info!(%id, generation, exit_code, "managed application exited normally");
        }
        Ok(())
    }

    async fn sleep(&self) -> Result<(), String> {
        if !matches!(self.state.read().await.domain, DomainState::Manager) {
            return Err("sleep button is only available in the manager".into());
        }
        self.state
            .write()
            .await
            .apply(Transition::Sleep)
            .map_err(|e| e.to_string())?;
        self.controller.release_wakelock()?;
        self.controller.suspend().await?;
        self.controller.acquire_wakelock()?;
        self.restart_runtime_and_wait().await?;
        self.state
            .write()
            .await
            .apply(Transition::Wake)
            .map_err(|e| e.to_string())
    }

    async fn restore_system(&self) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        let domain = self.state.read().await.domain.clone();
        if matches!(domain, DomainState::System) {
            return Ok(());
        }
        // Publish the transition before stopping the runtime so concurrent
        // power/control requests cannot begin a second managed generation
        // while display ownership is being returned to xochitl.
        self.state.write().await.domain = DomainState::RestoringSystem;
        self.controller.stop_and_wait(HOME_UNIT).await?;
        self.runtime_generations.write().await.clear();
        if let Err(error) = set_foreground_marker(None) {
            warn!(%error, "could not clear foreground marker during system restore");
        }
        if let Err(error) = self.set_power_grab(false).await {
            // A disconnected input thread has dropped its fd; a failed
            // ungrab is handled in-thread by closing and reopening the fd.
            warn!(%error, "could not confirm power-key release during system restore");
        }
        // Starting xochitl is the safety-critical step.  Never skip it merely
        // because a stale marker could not be removed or the input thread had
        // already gone away.
        self.controller.restore_system().await?;
        self.state
            .write()
            .await
            .apply(Transition::SystemReady)
            .map_err(|e| e.to_string())
    }

    async fn set_power_grab(&self, grab: bool) -> Result<(), String> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.power_control
            .send(power_device::Control::Grab {
                grab,
                reply: reply_tx,
            })
            .map_err(|error| format!("power input thread is unavailable: {error}"))?;
        tokio::time::timeout(std::time::Duration::from_secs(1), reply_rx)
            .await
            .map_err(|_| "power input grab acknowledgement timed out".to_string())?
            .map_err(|_| "power input thread closed without acknowledgement".to_string())?
    }

    async fn restart_runtime_and_wait(&self) -> Result<(), String> {
        self.runtime_generations.write().await.clear();
        self.controller.restart(HOME_UNIT).await?;
        wait_runtime_ready().await
    }

    async fn package(&self, operation: PackageOperation) -> Result<(), String> {
        if !matches!(self.state.read().await.domain, DomainState::Manager) {
            return Err("package operations are only available in the manager".into());
        }
        let encoded = serde_json::to_string(&operation).map_err(|e| e.to_string())?;
        self.controller
            .start_transient_worker("remagic-update", &[encoded])
            .await
    }
}

fn runtime_generation_matches(
    generations: &BTreeMap<AppId, u64>,
    app: &AppId,
    generation: u64,
) -> bool {
    generation != 0 && generations.get(app).copied() == Some(generation)
}

fn runtime_exit_is_crash(exit_code: i32, reported_crash: bool) -> bool {
    reported_crash || exit_code != 0
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

fn atomic_write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let temp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    fs::write(&temp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&temp, path).map_err(|e| e.to_string())
}

fn set_foreground_marker(app: Option<&AppId>) -> Result<(), String> {
    match app {
        Some(app) => fs::write(FOREGROUND_MARKER, format!("{}\n", app.as_str()))
            .map_err(|error| error.to_string()),
        None => match fs::remove_file(FOREGROUND_MARKER) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        },
    }
}

async fn wait_runtime_ready() -> Result<(), String> {
    for _ in 0..100 {
        if tokio::net::UnixStream::connect(runtime::SOCKET)
            .await
            .is_ok()
        {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    Err(format!(
        "runtime did not publish a ready socket at {} within 5 seconds",
        runtime::SOCKET
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_runtime_exit_cannot_match_replacement_generation() {
        let app = AppId::new("koreader").unwrap();
        let generations = BTreeMap::from([(app.clone(), 8)]);
        assert!(!runtime_generation_matches(&generations, &app, 7));
        assert!(runtime_generation_matches(&generations, &app, 8));
        assert!(!runtime_generation_matches(&generations, &app, 0));
    }

    #[test]
    fn runtime_exit_for_untracked_app_is_rejected() {
        let app = AppId::new("magicpaper").unwrap();
        assert!(!runtime_generation_matches(&BTreeMap::new(), &app, 1));
    }

    #[test]
    fn only_zero_normal_exit_is_clean() {
        assert!(!runtime_exit_is_crash(0, false));
        assert!(runtime_exit_is_crash(1, false));
        assert!(runtime_exit_is_crash(0, true));
    }
}
