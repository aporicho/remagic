use super::*;
use crate::{app_runtime, display_host};
use remagic_core::{DomainState, SessionStatus, Transition};
use remagic_protocol::AppCommand;
use std::time::Duration;
use tracing::warn;

mod recovery;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ForegroundFence {
    generation: u64,
    foreground_epoch: u64,
    lease_id: u64,
}

impl Daemon {
    pub(in crate::daemon) async fn park(
        &self,
        id: AppId,
        returning_system: bool,
        show_manager: bool,
    ) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        let manifest = self
            .manifests
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("unknown application {id}"))?;
        let previous_session = self.sessions.read().await.get(&id).cloned();
        let fence = self.foreground_fence_if_supported(&id, &manifest).await?;
        self.begin_park(&id, returning_system).await?;
        let mut recovery_payload = previous_session
            .as_ref()
            .and_then(|session| session.resume_payload.clone());
        let result = self
            .complete_park(&id, &manifest, fence, show_manager, &mut recovery_payload)
            .await;
        if let Err(cause) = result {
            let recovery = self
                .recover_failed_park(&id, &manifest, fence, recovery_payload)
                .await;
            return Err(match recovery {
                Ok(summary) => format!("{cause}; {summary}"),
                Err(error) => format!("{cause}; park recovery failed: {error}"),
            });
        }
        Ok(())
    }

    pub(super) async fn begin_park(
        &self,
        id: &AppId,
        returning_system: bool,
    ) -> Result<(), String> {
        let mut state = self.state.write().await;
        if !matches!(&state.domain, DomainState::Foreground(current) if current == id) {
            return Err(format!(
                "cannot park {id} while manager domain is {:?}",
                state.domain
            ));
        }
        state
            .apply(if returning_system {
                Transition::TriplePower
            } else {
                Transition::SinglePower
            })
            .map_err(|error| error.to_string())
    }

    async fn complete_park(
        &self,
        id: &AppId,
        manifest: &remagic_core::AppManifest,
        fence: Option<ForegroundFence>,
        show_manager: bool,
        recovery_payload: &mut Option<serde_json::Value>,
    ) -> Result<(), String> {
        let parked = self
            .send_background_if_supported(id, manifest, fence)
            .await?;
        let session = self.parked_session(id, manifest, parked).await;
        if session.resume_payload.is_some() {
            *recovery_payload = session.resume_payload.clone();
        }
        // Session durability is the semantic park acknowledgement. Do not
        // revoke the visible lease or expose Home until the exact foreground
        // token has reported both state_saved and background_ready.
        self.save_session(session).await?;
        self.state
            .read()
            .await
            .domain
            .eq(&DomainState::Parking(id.clone()))
            .then_some(())
            .ok_or_else(|| format!("park ownership for {id} changed before display handoff"))?;
        utils::set_foreground_marker(None)?;
        if show_manager {
            self.show_manager_surface(false).await?
        } else {
            display_host::clear_foreground().await?
        }
        // The lifecycle acknowledgement and durable session must precede
        // foreground lease revocation, and the application cgroup may only be
        // frozen after display/input have been handed away. A frozen unit is
        // still active and remains a resumable background task.
        if self
            .runtime_background_execution
            .read()
            .await
            .get(id)
            .copied()
            .ok_or_else(|| format!("application {id} lost its scheduling policy during park"))?
            .freezes_process()
        {
            self.controller
                .freeze_and_wait(&utils::app_unit(id))
                .await
                .map_err(|error| {
                    format!("could not freeze background application {id}: {error}")
                })?;
        }
        self.finish_park_transition(id.clone()).await
    }

    async fn foreground_fence_if_supported(
        &self,
        id: &AppId,
        manifest: &remagic_core::AppManifest,
    ) -> Result<Option<ForegroundFence>, String> {
        if !supports_lifecycle_v2(manifest) {
            return Ok(None);
        }
        let generation = self
            .runtime_generations
            .read()
            .await
            .get(id)
            .copied()
            .ok_or_else(|| format!("application {id} has no supervised generation"))?;
        let (foreground_epoch, lease_id) = self
            .runtime_foreground_fences
            .read()
            .await
            .get(id)
            .copied()
            .ok_or_else(|| format!("application {id} has no foreground lease"))?;
        Ok(Some(ForegroundFence {
            generation,
            foreground_epoch,
            lease_id,
        }))
    }

    async fn send_background_if_supported(
        &self,
        id: &AppId,
        manifest: &remagic_core::AppManifest,
        fence: Option<ForegroundFence>,
    ) -> Result<Option<app_runtime::LifecycleStatus>, String> {
        if !supports_lifecycle_v2(manifest) {
            return Ok(None);
        }
        let runtime_dir = manifest
            .runtime
            .directories
            .as_ref()
            .map(|dirs| dirs.runtime_dir.as_path())
            .ok_or_else(|| format!("application {id} has no runtime directory"))?;
        let fence = fence.ok_or_else(|| format!("application {id} lost its foreground fence"))?;
        app_runtime::command(runtime_dir, &AppCommand::EnterBackground).await?;
        let timeout = Duration::from_millis(manifest.shutdown.graceful_timeout_ms.max(100));
        app_runtime::wait_background_ready(
            runtime_dir,
            id,
            fence.generation,
            fence.foreground_epoch,
            fence.lease_id,
            timeout,
        )
        .await
        .map(Some)
    }

    async fn parked_session(
        &self,
        id: &AppId,
        manifest: &remagic_core::AppManifest,
        lifecycle: Option<app_runtime::LifecycleStatus>,
    ) -> AppSession {
        let semantic_lifecycle = lifecycle.is_some();
        let title = lifecycle
            .as_ref()
            .and_then(|status| status.title.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| manifest.name.clone());
        let subtitle = lifecycle
            .as_ref()
            .and_then(|status| status.subtitle.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "已暂停，可继续".into());
        let previous_payload = self
            .sessions
            .read()
            .await
            .get(id)
            .and_then(|value| value.resume_payload.clone());
        let resume_payload = lifecycle
            .and_then(|status| status.resume_payload)
            .or(previous_payload)
            .or_else(|| (!semantic_lifecycle).then(|| serde_json::json!({ "parked": true })));
        AppSession {
            schema: 1,
            app_id: id.clone(),
            status: background_session_status(manifest.is_resident()),
            title,
            subtitle,
            resume_payload,
            updated_at: utils::unix_now(),
            last_error: None,
        }
    }

    async fn finish_park_transition(&self, id: AppId) -> Result<(), String> {
        utils::set_foreground_marker(None)?;
        let mut state = self.state.write().await;
        match &state.domain {
            DomainState::Parking(expected) if expected == &id => state
                .apply(Transition::AppParked(id))
                .map_err(|e| e.to_string()),
            DomainState::Manager => Ok(()),
            current => Err(format!(
                "application stopped in unexpected state: {current:?}"
            )),
        }
    }

    pub(in crate::daemon) async fn record_parked(&self, session: AppSession) -> Result<(), String> {
        let id = session.app_id.clone();
        if self
            .manifests
            .read()
            .await
            .get(&id)
            .is_some_and(supports_lifecycle_v2)
        {
            // lifecycle:v2 parking is committed only by the fenced status-file
            // waiter in `park`. The runner's legacy callback may arrive after
            // a timeout and must never complete a newer Parking epoch.
            warn!(%id, "ignored compatibility application-parked callback for lifecycle:v2 app");
            return Ok(());
        }
        if !matches!(&self.state.read().await.domain, DomainState::Parking(expected) if expected == &id)
        {
            warn!(%id, "ignored stale application-parked event");
            return Ok(());
        }
        self.save_session(session).await?;
        utils::set_foreground_marker(None)?;
        let mut state = self.state.write().await;
        if matches!(&state.domain, DomainState::Parking(expected) if expected == &id) {
            state
                .apply(Transition::AppParked(id))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

fn background_session_status(resident: bool) -> SessionStatus {
    if resident {
        SessionStatus::Background
    } else {
        SessionStatus::Parked
    }
}

fn supports_lifecycle_v2(manifest: &remagic_core::AppManifest) -> bool {
    manifest
        .capabilities
        .iter()
        .any(|capability| capability.as_str() == "lifecycle:v2")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_resident_process_is_reported_as_background_not_parked() {
        assert_eq!(background_session_status(true), SessionStatus::Background);
        assert_eq!(background_session_status(false), SessionStatus::Parked);
    }
}
