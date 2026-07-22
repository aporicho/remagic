use super::*;
use crate::display_host;
use remagic_core::{AppManifest, AppToken, DomainState};
use remagic_protocol::InputMode;
use std::future::Future;

mod policy;

use policy::{
    has_capability, initial_input_mode, supports_dynamic_input_mode, DIRECT_INK_CAPABILITY,
    DYNAMIC_INPUT_MODE_CAPABILITY,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeInputState {
    pub(super) generation: u64,
    pub(super) foreground_epoch: u64,
    pub(super) lease_id: u64,
    pub(super) mode: InputMode,
    /// Capabilities are fixed to the supervised generation. A manifest reload
    /// cannot revoke or elevate a process which is already running.
    pub(super) dynamic_allowed: bool,
    pub(super) direct_ink_allowed: bool,
    /// Pending fences may be updated by the application without acquiring the
    /// outer transition lock. The launch/recovery transaction applies the last
    /// value while holding this map's write lock, then flips this bit before
    /// exposing the foreground state.
    pub(super) pending: bool,
}

impl RuntimeInputState {
    pub(super) fn pending(token: &AppToken, manifest: &AppManifest) -> Result<Self, String> {
        token.validate().map_err(|error| error.to_string())?;
        let lease_id = token
            .lease_id
            .ok_or_else(|| "input-mode token has no display lease".to_string())?;
        if token.foreground_epoch == 0 {
            return Err("input-mode token has zero foreground epoch".into());
        }
        Ok(Self {
            generation: token.generation,
            foreground_epoch: token.foreground_epoch,
            lease_id,
            mode: initial_input_mode(manifest),
            dynamic_allowed: supports_dynamic_input_mode(manifest),
            direct_ink_allowed: has_capability(manifest, DIRECT_INK_CAPABILITY),
            pending: true,
        })
    }

    pub(super) fn matches(self, token: &AppToken) -> bool {
        self.generation == token.generation
            && self.foreground_epoch == token.foreground_epoch
            && token.lease_id == Some(self.lease_id)
    }

    fn validate_mode(self, id: &AppId, mode: InputMode) -> Result<bool, String> {
        if !self.dynamic_allowed {
            return Err(format!(
                "application {id} cannot negotiate input mode without {DYNAMIC_INPUT_MODE_CAPABILITY}"
            ));
        }
        let ink_enabled = mode.ink_enabled();
        if ink_enabled && !self.direct_ink_allowed {
            return Err(format!(
                "application {id} cannot enter writing mode without {DIRECT_INK_CAPABILITY}"
            ));
        }
        Ok(ink_enabled)
    }
}

impl Daemon {
    pub(super) async fn set_input_mode(
        &self,
        token: &AppToken,
        mode: InputMode,
    ) -> Result<bool, String> {
        self.set_input_mode_with(token, mode, display_host::configure_ink)
            .await
    }

    async fn set_input_mode_with<Configure, ConfigureFuture>(
        &self,
        token: &AppToken,
        mode: InputMode,
        configure: Configure,
    ) -> Result<bool, String>
    where
        Configure: FnOnce(i32, u64, u64, bool) -> ConfigureFuture,
        ConfigureFuture: Future<Output = Result<(), String>>,
    {
        token.validate().map_err(|error| error.to_string())?;
        let id = &token.app_id;
        // Launch and failed-park recovery both wait for an application event
        // while holding the transition lock. A request carrying the exact
        // pending fence must therefore update the pending record directly. The
        // map lock linearizes this with foreground commit and all cleanup.
        let domain = self.state.read().await.domain.clone();
        if matches!(&domain, DomainState::Launching(current) | DomainState::Parking(current) if current == id)
        {
            let mut modes = self.runtime_input_modes.write().await;
            let still_pending = matches!(
                &self.state.read().await.domain,
                DomainState::Launching(current) | DomainState::Parking(current) if current == id
            );
            if still_pending {
                let state = modes
                    .get_mut(id)
                    .ok_or_else(|| format!("application {id} has no pending input fence"))?;
                if !state.matches(token) {
                    return Err(format!(
                        "application {id} supplied a stale input-mode token"
                    ));
                }
                if !state.pending {
                    return Err(format!("application {id} input fence is not pending"));
                }
                let ink_enabled = state.validate_mode(id, mode)?;
                state.mode = mode;
                return Ok(ink_enabled);
            }
            drop(modes);
        }

        // An already-foreground application changes the display host while
        // holding the transition lock, keeping validation and application
        // atomic with park, close, and app-switch operations.
        let _guard = self.transition_lock.lock().await;
        let domain = self.state.read().await.domain.clone();
        if !matches!(&domain, DomainState::Foreground(current) if current == id) {
            return Err(format!(
                "application {id} is not the current foreground application"
            ));
        }

        let mut modes = self.runtime_input_modes.write().await;
        let state = modes
            .get_mut(id)
            .ok_or_else(|| format!("application {id} has no active input fence"))?;
        if !state.matches(token) {
            return Err(format!(
                "application {id} supplied a stale input-mode token"
            ));
        }
        if state.pending {
            return Err(format!("application {id} input fence is still pending"));
        }
        let ink_enabled = state.validate_mode(id, mode)?;
        configure(
            display_host::app_surface_key(id),
            token.generation,
            token.foreground_epoch,
            ink_enabled,
        )
        .await?;
        state.mode = mode;
        Ok(ink_enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use remagic_core::{AppManifest, ManagerState, ManifestStore, SessionStore};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicBool, AtomicU64};
    use std::sync::{Arc, Mutex as StdMutex};

    fn manifest() -> AppManifest {
        toml::from_str(include_str!("../../../../manifests/magicpaper.toml")).unwrap()
    }

    fn daemon_with(manifest: AppManifest, foreground: AppId) -> Daemon {
        let id = manifest.id.clone();
        let token = token(&id);
        let dynamic_allowed = supports_dynamic_input_mode(&manifest);
        let direct_ink_allowed = has_capability(&manifest, DIRECT_INK_CAPABILITY);
        let root = std::env::temp_dir().join(format!(
            "remagic-input-mode-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let (events, _event_rx) = tokio::sync::mpsc::channel(1);
        let (power_control, _power_rx) = std::sync::mpsc::channel();
        Daemon {
            state: RwLock::new(ManagerState {
                domain: DomainState::Foreground(foreground),
                last_app: Some(id.clone()),
                sequence: 1,
            }),
            manifests: RwLock::new(BTreeMap::from([(id.clone(), manifest)])),
            sessions: RwLock::new(BTreeMap::new()),
            runtime_generations: RwLock::new(BTreeMap::from([(id.clone(), 17)])),
            runtime_foreground_fences: RwLock::new(BTreeMap::from([(id.clone(), (23, 23))])),
            runtime_input_modes: RwLock::new(BTreeMap::from([(
                id,
                RuntimeInputState {
                    generation: token.generation,
                    foreground_epoch: token.foreground_epoch,
                    lease_id: token.lease_id.unwrap(),
                    mode: InputMode::AnimationLocked,
                    dynamic_allowed,
                    direct_ink_allowed,
                    pending: false,
                },
            )])),
            runtime_exit_reports: RwLock::new(BTreeMap::new()),
            runtime_missing_observations: RwLock::new(BTreeMap::new()),
            session_store: SessionStore::new(root.clone()),
            manifest_store: ManifestStore::new(root.join("manifests")),
            controller: crate::system::SystemController::new(),
            transition_lock: Mutex::new(()),
            events,
            power_control,
            next_generation: AtomicU64::new(1),
            next_foreground_epoch: AtomicU64::new(1),
            next_sleep_epoch: AtomicU64::new(1),
            sleep_transaction: sleep::SleepTransaction::default(),
            launch_interrupt_epoch: Arc::new(AtomicU64::new(1)),
            manager_repair_pending: AtomicBool::new(false),
            domain_recovery_pending: AtomicBool::new(false),
        }
    }

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

    fn token(id: &AppId) -> AppToken {
        AppToken {
            app_id: id.clone(),
            generation: 17,
            foreground_epoch: 23,
            lease_id: Some(23),
        }
    }

    #[tokio::test]
    async fn writing_uses_the_current_runtime_fence_and_enables_direct_ink() {
        let manifest = manifest();
        let id = manifest.id.clone();
        let daemon = daemon_with(manifest, id.clone());
        let token = token(&id);
        let observed = Arc::new(StdMutex::new(None));
        let captured = Arc::clone(&observed);

        let enabled = daemon
            .set_input_mode_with(
                &token,
                InputMode::Writing,
                move |key, generation, epoch, ink| {
                    *captured.lock().unwrap() = Some((key, generation, epoch, ink));
                    async { Ok(()) }
                },
            )
            .await
            .unwrap();

        assert!(enabled);
        assert_eq!(
            *observed.lock().unwrap(),
            Some((display_host::app_surface_key(&id), 17, 23, true))
        );
        assert_eq!(
            daemon.runtime_input_modes.read().await.get(&id).copied(),
            Some(RuntimeInputState {
                generation: 17,
                foreground_epoch: 23,
                lease_id: 23,
                mode: InputMode::Writing,
                dynamic_allowed: true,
                direct_ink_allowed: true,
                pending: false,
            })
        );
    }

    #[tokio::test]
    async fn launching_app_records_mode_without_waiting_for_transition_lock() {
        let manifest = manifest();
        let id = manifest.id.clone();
        let daemon = daemon_with(manifest, id.clone());
        let token = token(&id);
        daemon.state.write().await.domain = DomainState::Launching(id.clone());
        daemon
            .runtime_input_modes
            .write()
            .await
            .get_mut(&id)
            .unwrap()
            .pending = true;
        let configure_called = Arc::new(AtomicBool::new(false));
        let captured = Arc::clone(&configure_called);
        let _launch_guard = daemon.transition_lock.lock().await;

        let enabled = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            daemon.set_input_mode_with(&token, InputMode::Writing, move |_, _, _, _| {
                captured.store(true, Ordering::Release);
                async { Ok(()) }
            }),
        )
        .await
        .expect("startup mode request must not wait on its own launch transaction")
        .unwrap();

        assert!(enabled);
        assert!(!configure_called.load(Ordering::Acquire));
        assert_eq!(
            daemon.runtime_input_modes.read().await.get(&id).copied(),
            Some(RuntimeInputState {
                generation: 17,
                foreground_epoch: 23,
                lease_id: 23,
                mode: InputMode::Writing,
                dynamic_allowed: true,
                direct_ink_allowed: true,
                pending: true,
            })
        );
    }

    #[tokio::test]
    async fn stale_generation_epoch_lease_and_app_never_reach_display_io() {
        let base_manifest = manifest();
        let id = base_manifest.id.clone();
        let other = AppId::new("koreader").unwrap();
        let stale_tokens = [
            AppToken {
                generation: 16,
                ..token(&id)
            },
            AppToken {
                foreground_epoch: 22,
                ..token(&id)
            },
            AppToken {
                lease_id: Some(22),
                ..token(&id)
            },
            AppToken {
                app_id: other,
                ..token(&id)
            },
        ];
        for stale in stale_tokens {
            let daemon = daemon_with(base_manifest.clone(), id.clone());
            let before = daemon.runtime_input_modes.read().await.get(&id).copied();
            let called = Arc::new(AtomicBool::new(false));
            let captured = Arc::clone(&called);
            assert!(daemon
                .set_input_mode_with(&stale, InputMode::Writing, move |_, _, _, _| {
                    captured.store(true, Ordering::Release);
                    async { Ok(()) }
                })
                .await
                .is_err());
            assert!(!called.load(Ordering::Acquire));
            assert_eq!(
                daemon.runtime_input_modes.read().await.get(&id).copied(),
                before
            );
        }
    }

    #[tokio::test]
    async fn active_generation_capabilities_do_not_drift_on_manifest_reload() {
        let app = manifest();
        let id = app.id.clone();
        let daemon = daemon_with(app, id.clone());
        daemon
            .manifests
            .write()
            .await
            .get_mut(&id)
            .unwrap()
            .capabilities
            .clear();
        assert!(daemon
            .set_input_mode_with(&token(&id), InputMode::Writing, |_, _, _, _| async {
                Ok(())
            })
            .await
            .unwrap());

        let mut no_ink = manifest();
        no_ink
            .capabilities
            .retain(|capability| capability.as_str() != DIRECT_INK_CAPABILITY);
        let daemon = daemon_with(no_ink, id.clone());
        daemon
            .manifests
            .write()
            .await
            .get_mut(&id)
            .unwrap()
            .capabilities
            .push(remagic_core::Capability::new(DIRECT_INK_CAPABILITY).unwrap());
        let called = Arc::new(AtomicBool::new(false));
        let captured = Arc::clone(&called);
        assert!(daemon
            .set_input_mode_with(&token(&id), InputMode::Writing, move |_, _, _, _| {
                captured.store(true, Ordering::Release);
                async { Ok(()) }
            })
            .await
            .is_err());
        assert!(!called.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn non_writing_modes_disable_ink_without_requiring_the_capability() {
        for mode in [InputMode::AnimationLocked, InputMode::Modal] {
            let mut app = manifest();
            app.capabilities
                .retain(|capability| capability.as_str() != DIRECT_INK_CAPABILITY);
            let id = app.id.clone();
            let daemon = daemon_with(app, id.clone());
            let token = token(&id);
            let enabled = daemon
                .set_input_mode_with(&token, mode, |_key, _generation, _epoch, ink| async move {
                    assert!(!ink);
                    Ok(())
                })
                .await
                .unwrap();
            assert!(!enabled);
        }
    }

    #[tokio::test]
    async fn a_background_app_cannot_change_the_foreground_input_mode() {
        let app = manifest();
        let id = app.id.clone();
        let daemon = daemon_with(app, AppId::new("koreader").unwrap());
        let token = token(&id);
        let called = Arc::new(AtomicBool::new(false));
        let captured = Arc::clone(&called);
        let error = daemon
            .set_input_mode_with(&token, InputMode::Writing, move |_, _, _, _| {
                captured.store(true, Ordering::Release);
                async { Ok(()) }
            })
            .await
            .unwrap_err();
        assert!(error.contains("not the current foreground"));
        assert!(!called.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn writing_without_direct_ink_capability_is_rejected_before_display_io() {
        let mut app = manifest();
        app.capabilities
            .retain(|capability| capability.as_str() != DIRECT_INK_CAPABILITY);
        let id = app.id.clone();
        let daemon = daemon_with(app, id.clone());
        let token = token(&id);
        let called = Arc::new(AtomicBool::new(false));
        let captured = Arc::clone(&called);
        let error = daemon
            .set_input_mode_with(&token, InputMode::Writing, move |_, _, _, _| {
                captured.store(true, Ordering::Release);
                async { Ok(()) }
            })
            .await
            .unwrap_err();
        assert!(error.contains(DIRECT_INK_CAPABILITY));
        assert!(!called.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn display_host_rejection_is_not_acknowledged_as_an_applied_mode() {
        let app = manifest();
        let id = app.id.clone();
        let daemon = daemon_with(app, id.clone());
        let token = token(&id);
        let error = daemon
            .set_input_mode_with(&token, InputMode::Modal, |_, _, _, _| async {
                Err("display host did not retain ink fence".into())
            })
            .await
            .unwrap_err();
        assert!(error.contains("did not retain ink fence"));
    }
}
