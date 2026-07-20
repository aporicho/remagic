mod power_device;
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
const HOME_UNIT: &str = "remagic-home.service";
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
    AppReady(AppId),
    AppParked(AppSession),
    Package(PackageOperation),
    ReloadManifests,
}

struct Daemon {
    state: RwLock<ManagerState>,
    manifests: RwLock<BTreeMap<AppId, remagic_core::AppManifest>>,
    sessions: RwLock<BTreeMap<AppId, AppSession>>,
    session_store: SessionStore,
    manifest_store: ManifestStore,
    controller: SystemController,
    transition_lock: Mutex<()>,
    events: mpsc::Sender<Event>,
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
        if !matches!(signal_daemon.state.read().await.domain, DomainState::System) {
            let _ = signal_daemon.restore_system().await;
        }
        std::process::exit(0);
    });

    while let Some(event) = event_rx.recv().await {
        if let Err(error) = daemon.handle_event(event).await {
            error!(%error, "transition failed");
            if !matches!(daemon.state.read().await.domain, DomainState::System) {
                let _ = daemon.restore_system().await;
            }
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
        match self.events.send(event).await {
            Ok(()) => Response::Ok,
            Err(_) => Response::Error {
                message: "manager event loop is unavailable".into(),
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
            Event::AppReady(id) => {
                let mut state = self.state.write().await;
                state
                    .apply(Transition::AppReady(id))
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
                    self.controller.restart(HOME_UNIT).await?;
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
        self.power_control
            .send(power_device::Control::Grab(true))
            .map_err(|e| e.to_string())?;
        self.controller.enter_managed().await?;
        set_foreground_marker(None)?;
        self.controller.start(HOME_UNIT).await?;
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
        self.controller.stop(HOME_UNIT).await?;
        self.controller.start(&app_unit(&id)).await
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
        self.controller.stop(&app_unit(&id)).await?;
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
        let manager_needs_start = match &state.domain {
            DomainState::Parking(expected) if expected == &id => {
                state
                    .apply(Transition::AppParked(id))
                    .map_err(|e| e.to_string())?;
                true
            }
            // The runner may report its parked session while systemd is still
            // stopping the unit. In that race record_parked already completed
            // the transition and started the manager.
            DomainState::Manager => true,
            current => {
                return Err(format!(
                    "application stopped in unexpected state: {current:?}"
                ))
            }
        };
        drop(state);
        if !returning_system && show_manager && manager_needs_start {
            self.controller.start(HOME_UNIT).await?;
        }
        Ok(())
    }

    async fn record_parked(&self, session: AppSession) -> Result<(), String> {
        let id = session.app_id.clone();
        self.save_session(session).await?;
        set_foreground_marker(None)?;
        let mut state = self.state.write().await;
        let start_manager = match &state.domain {
            DomainState::Parking(expected) if expected == &id => {
                state
                    .apply(Transition::AppParked(id))
                    .map_err(|e| e.to_string())?;
                false
            }
            DomainState::Foreground(expected) | DomainState::Launching(expected)
                if expected == &id =>
            {
                state
                    .apply(Transition::AppExited(id))
                    .map_err(|e| e.to_string())?;
                true
            }
            _ => return Ok(()),
        };
        drop(state);
        if start_manager {
            self.controller.start(HOME_UNIT).await
        } else {
            Ok(())
        }
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
            self.controller.stop(&app_unit(&id)).await?;
            self.controller.start(HOME_UNIT).await?;
            self.state.write().await.domain = DomainState::Manager;
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

    async fn sleep(&self) -> Result<(), String> {
        if !matches!(self.state.read().await.domain, DomainState::Manager) {
            return Err("sleep button is only available in the manager".into());
        }
        self.state
            .write()
            .await
            .apply(Transition::Sleep)
            .map_err(|e| e.to_string())?;
        self.controller.release_wakelock();
        self.controller.suspend().await?;
        self.controller.acquire_wakelock();
        self.state
            .write()
            .await
            .apply(Transition::Wake)
            .map_err(|e| e.to_string())?;
        self.controller.restart(HOME_UNIT).await
    }

    async fn restore_system(&self) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        let domain = self.state.read().await.domain.clone();
        if matches!(domain, DomainState::System) {
            return Ok(());
        }
        if let DomainState::Foreground(id) | DomainState::Launching(id) | DomainState::Parking(id) =
            domain
        {
            let _ = self.controller.stop(&app_unit(&id)).await;
        }
        let _ = self.controller.stop(HOME_UNIT).await;
        set_foreground_marker(None)?;
        self.state.write().await.domain = DomainState::RestoringSystem;
        self.power_control
            .send(power_device::Control::Grab(false))
            .map_err(|e| e.to_string())?;
        self.controller.restore_system().await?;
        self.state
            .write()
            .await
            .apply(Transition::SystemReady)
            .map_err(|e| e.to_string())
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

fn app_unit(id: &AppId) -> String {
    format!("remagic-app@{}.service", id.as_str())
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
