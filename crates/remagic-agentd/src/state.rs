use crate::backend::{provider_configured, WorkerHandle};
use remagic_core::{AppId, DeviceProfile};
use remagic_protocol::{AgentErrorCode, AgentLane, AgentProfile, AgentRuntimeSource, AgentStatus};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{watch, Mutex};

#[derive(Clone)]
pub(crate) struct AgentState {
    inner: Arc<Mutex<BTreeMap<AppId, AppState>>>,
    device: DeviceProfile,
    pi_binary: PathBuf,
}

#[derive(Default)]
struct AppState {
    profile: Option<AgentProfile>,
    system_prompt: String,
    worker: Option<WorkerHandle>,
    active: Option<ActiveTurn>,
    principals: BTreeMap<String, PrincipalAuth>,
}

#[derive(Default)]
struct PrincipalAuth {
    generation: u64,
    client_token: String,
}

struct ActiveTurn {
    id: String,
    lane: AgentLane,
    principal: String,
    cancel: watch::Sender<bool>,
}

#[derive(Debug)]
pub(crate) struct StartedTurn {
    pub id: String,
    pub cancel: watch::Receiver<bool>,
    pub worker: WorkerHandle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClientIdentity {
    pub app_id: AppId,
    pub generation: u64,
    pub principal: String,
}

impl AgentState {
    pub fn new(device: DeviceProfile, pi_binary: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BTreeMap::new())),
            device,
            pi_binary,
        }
    }

    pub async fn status(&self, app_id: &AppId) -> AgentStatus {
        let states = self.inner.lock().await;
        let state = states.get(app_id);
        AgentStatus {
            available: pi_available(&self.pi_binary),
            provider_configured: state
                .and_then(|value| value.profile.as_ref())
                .is_some_and(|profile| provider_configured(&profile.provider)),
            runtime_source: runtime_source(&self.pi_binary),
            busy: state.and_then(|value| value.active.as_ref()).is_some(),
            active_turn_id: state
                .and_then(|value| value.active.as_ref())
                .map(|turn| turn.id.clone()),
            profile: state.and_then(|value| value.profile.clone()),
            device: self.device.clone(),
        }
    }

    pub async fn start(
        &self,
        app_id: &AppId,
        request_id: &str,
        profile: AgentProfile,
        system_prompt: &str,
        lane: AgentLane,
        principal: &str,
    ) -> Result<StartedTurn, AgentErrorCode> {
        if !pi_available(&self.pi_binary) {
            return Err(AgentErrorCode::Unavailable);
        }
        loop {
            self.preempt_for_interaction(app_id, lane).await?;
            let mut states = self.inner.lock().await;
            let state = states.entry(app_id.clone()).or_default();
            if let Some(active) = state.active.as_ref() {
                if lane == AgentLane::Interactive && active.lane != AgentLane::Interactive {
                    drop(states);
                    continue;
                }
                return Err(AgentErrorCode::Busy);
            }
            let worker_changed = state.profile.as_ref() != Some(&profile)
                || state.system_prompt != system_prompt
                || state.worker.as_ref().is_some_and(WorkerHandle::is_closed);
            if worker_changed {
                if let Some(worker) = state.worker.take() {
                    worker.shutdown().await;
                }
            }
            if state.worker.is_none() {
                let worker = WorkerHandle::spawn(&self.pi_binary, app_id, &profile, system_prompt)
                    .await
                    .map_err(|error| {
                        #[cfg(test)]
                        eprintln!("test Pi worker startup failed for {app_id}: {error}");
                        tracing::warn!(app_id = %app_id, %error, "could not start Pi worker");
                        AgentErrorCode::BackendFailed
                    })?;
                state.worker = Some(worker);
            }
            let turn_id = format!("{}:{}", app_id, request_id);
            let (cancel, receiver) = watch::channel(false);
            state.profile = Some(profile);
            state.system_prompt = system_prompt.into();
            state.active = Some(ActiveTurn {
                id: turn_id.clone(),
                lane,
                principal: principal.into(),
                cancel,
            });
            return Ok(StartedTurn {
                id: turn_id,
                cancel: receiver,
                worker: state.worker.as_ref().expect("worker was created").clone(),
            });
        }
    }

    async fn preempt_for_interaction(
        &self,
        app_id: &AppId,
        lane: AgentLane,
    ) -> Result<(), AgentErrorCode> {
        let cancel = {
            let states = self.inner.lock().await;
            let active = states.get(app_id).and_then(|state| state.active.as_ref());
            match active {
                None => return Ok(()),
                Some(active)
                    if lane == AgentLane::Interactive && active.lane != AgentLane::Interactive =>
                {
                    active.cancel.clone()
                }
                Some(_) => return Err(AgentErrorCode::Busy),
            }
        };
        let _ = cancel.send(true);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let active = self
                    .inner
                    .lock()
                    .await
                    .get(app_id)
                    .is_some_and(|state| state.active.is_some());
                if !active {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .map_err(|_| AgentErrorCode::Busy)
    }

    pub async fn finish(&self, app_id: &AppId, turn_id: &str) {
        let mut states = self.inner.lock().await;
        if let Some(state) = states.get_mut(app_id) {
            if state.active.as_ref().is_some_and(|turn| turn.id == turn_id) {
                state.active = None;
            }
        }
    }

    pub async fn cancel(&self, app_id: &AppId, turn_id: &str) -> bool {
        let states = self.inner.lock().await;
        states
            .get(app_id)
            .and_then(|state| state.active.as_ref())
            .filter(|turn| turn.id == turn_id)
            .is_some_and(|turn| turn.cancel.send(true).is_ok())
    }

    pub async fn reload_profile(
        &self,
        app_id: &AppId,
        profile: Option<AgentProfile>,
    ) -> Result<(), AgentErrorCode> {
        let mut states = self.inner.lock().await;
        let state = states.entry(app_id.clone()).or_default();
        if state.active.is_some() {
            return Err(AgentErrorCode::Busy);
        }
        // The paper settings page uses a same-profile reload as a credential
        // and runtime check. Keep an already-warm Pi session in that case;
        // only an actual profile change or an explicit None restart tears it
        // down.
        if profile.is_some() && profile.as_ref() == state.profile.as_ref() {
            return Ok(());
        }
        if let Some(worker) = state.worker.take() {
            worker.shutdown().await;
        }
        if let Some(profile) = profile {
            state.profile = Some(profile);
        }
        Ok(())
    }

    pub async fn new_session(&self, app_id: &AppId) -> Result<(), AgentErrorCode> {
        let states = self.inner.lock().await;
        let Some(state) = states.get(app_id) else {
            // No worker means there is no resident context to clear.
            return Ok(());
        };
        if state.active.is_some() {
            return Err(AgentErrorCode::Busy);
        }
        let Some(worker) = state.worker.clone() else {
            return Ok(());
        };
        // Keep the application-state lock until Pi acknowledges the reset.
        // Otherwise a foreground start can enqueue a turn in the gap and race
        // with the session boundary the user explicitly requested.
        worker
            .new_session()
            .await
            .map_err(|_| AgentErrorCode::BackendFailed)
    }

    pub async fn authorize(&self, identity: &ClientIdentity, app_id: &AppId, token: &str) -> bool {
        if &identity.app_id != app_id {
            return false;
        }
        let mut states = self.inner.lock().await;
        let state = states.entry(app_id.clone()).or_default();
        let principal = state
            .principals
            .entry(identity.principal.clone())
            .or_default();
        match principal.generation {
            generation if generation > identity.generation => false,
            generation if generation == identity.generation => principal.client_token == token,
            _ if state
                .active
                .as_ref()
                .is_some_and(|turn| turn.principal == identity.principal) =>
            {
                false
            }
            _ => {
                principal.generation = identity.generation;
                principal.client_token = token.into();
                true
            }
        }
    }
}

fn runtime_source(path: &Path) -> AgentRuntimeSource {
    if !pi_available(path) {
        AgentRuntimeSource::Missing
    } else if path == Path::new("/home/root/apps/remagic/runtime/pi/bin/pi") {
        AgentRuntimeSource::Packaged
    } else if path == Path::new("/home/root/node/bin/pi") {
        AgentRuntimeSource::Legacy
    } else {
        AgentRuntimeSource::Override
    }
}

fn pi_available(path: &Path) -> bool {
    if path == Path::new("/home/root/apps/remagic/runtime/pi/bin/pi") {
        secure_packaged_file(path, true)
            && secure_packaged_file(
                Path::new("/home/root/apps/remagic/runtime/pi/bin/node"),
                true,
            )
            && secure_packaged_file(
                Path::new("/home/root/apps/remagic/runtime/pi/runtime.env"),
                false,
            )
    } else {
        executable(path)
    }
}

fn secure_packaged_file(path: &Path, require_executable: bool) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    std::fs::symlink_metadata(path).is_ok_and(|metadata| {
        !metadata.file_type().is_symlink()
            && metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.permissions().mode() & 0o022 == 0
            && (!require_executable || metadata.permissions().mode() & 0o111 != 0)
    })
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
#[path = "state/tests.rs"]
mod tests;
