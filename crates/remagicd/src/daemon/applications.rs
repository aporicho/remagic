use super::*;
use crate::app_runtime;
use remagic_core::{BackgroundService, DomainState, SessionStatus, Transition};
use remagic_protocol::AppCommand;
use tracing::{info, warn};

mod park;

impl Daemon {
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

    /// A resident session is `Background` only while its process generation
    /// remains live. Forced rollback and stock-domain restoration retain the
    /// resume snapshot but must report it as a cold `Parked` session.
    pub(super) async fn mark_session_process_stopped(&self, id: &AppId) -> Result<(), String> {
        let Some(mut session) = self.sessions.read().await.get(id).cloned() else {
            return Ok(());
        };
        if session.status != SessionStatus::Background {
            return Ok(());
        }
        session.status = SessionStatus::Parked;
        session.updated_at = utils::unix_now();
        self.save_session(session).await
    }

    pub(super) async fn close(&self, id: AppId, complete: bool) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        let domain = self.state.read().await.domain.clone();
        let was_foreground = matches!(&domain, DomainState::Foreground(current) if current == &id);
        let manifest = self.manifests.read().await.get(&id).cloned();
        let controlled_exit = self.reserve_controlled_exit(&id).await;
        let result = async {
            self.thaw_before_shutdown(&id).await?;
            self.request_shutdown(&id, manifest.as_ref()).await;
            if was_foreground {
                self.reveal_manager_before_close().await?
            }
            self.controller.stop_and_wait(&utils::app_unit(&id)).await?;
            self.clear_runtime_tracking(&id).await;
            {
                let mut state = self.state.write().await;
                if was_foreground {
                    state
                        .apply(Transition::AppExited(id.clone()))
                        .map_err(|error| error.to_string())?;
                }
                clear_closed_last_app(&mut state, &id);
            }
            self.session_store.remove(&id).map_err(|e| e.to_string())?;
            self.sessions.write().await.remove(&id);
            if complete {
                self.stop_background_service(manifest).await?
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            self.release_controlled_exit(&id, controlled_exit).await;
        }
        result
    }

    async fn reserve_controlled_exit(&self, id: &AppId) -> Option<PendingExit> {
        let generation = self.runtime_generations.read().await.get(id).copied()?;
        let pending = PendingExit {
            generation,
            source: ExitReportSource::Controlled,
        };
        self.runtime_exit_reports
            .write()
            .await
            .insert(id.clone(), pending);
        Some(pending)
    }

    async fn release_controlled_exit(&self, id: &AppId, pending: Option<PendingExit>) {
        let Some(pending) = pending else { return };
        let mut reports = self.runtime_exit_reports.write().await;
        if reports.get(id) == Some(&pending) {
            reports.remove(id);
        }
    }

    async fn thaw_before_shutdown(&self, id: &AppId) -> Result<(), String> {
        if !self
            .runtime_background_execution
            .read()
            .await
            .get(id)
            .copied()
            .is_some_and(remagic_core::BackgroundExecution::freezes_process)
        {
            return Ok(());
        }
        let unit = utils::app_unit(id);
        if self.controller.is_active_checked(&unit).await? {
            self.controller
                .thaw_and_wait(&unit)
                .await
                .map_err(|error| format!("could not thaw {id} before shutdown: {error}"))?;
        }
        Ok(())
    }

    async fn request_shutdown(&self, id: &AppId, manifest: Option<&remagic_core::AppManifest>) {
        if !self.controller.is_active(&utils::app_unit(id)).await {
            return;
        }
        let Some(runtime_dir) = manifest
            .and_then(|value| value.runtime.directories.as_ref())
            .map(|dirs| dirs.runtime_dir.as_path())
        else {
            return;
        };
        if let Err(error) = app_runtime::command(runtime_dir, &AppCommand::Shutdown).await {
            warn!(%id, %error, "application did not accept shutdown; systemd will enforce deadline");
        }
    }

    async fn reveal_manager_before_close(&self) -> Result<(), String> {
        self.state
            .write()
            .await
            .apply(Transition::SinglePower)
            .map_err(|e| e.to_string())?;
        utils::set_foreground_marker(None)?;
        self.show_manager_surface().await
    }

    async fn clear_runtime_tracking(&self, id: &AppId) {
        self.runtime_generations.write().await.remove(id);
        self.runtime_background_execution.write().await.remove(id);
        self.runtime_foreground_fences.write().await.remove(id);
        self.runtime_input_modes.write().await.remove(id);
        self.runtime_exit_reports.write().await.remove(id);
        self.runtime_missing_observations.write().await.remove(id);
    }

    async fn stop_background_service(
        &self,
        manifest: Option<remagic_core::AppManifest>,
    ) -> Result<(), String> {
        let Some(manifest) = manifest else {
            return Ok(());
        };
        match manifest.effective_background_service() {
            Some(BackgroundService::Systemd { unit }) => {
                self.controller.stop_and_wait(&unit).await?;
            }
            Some(BackgroundService::Managed { .. }) => {
                self.controller
                    .stop_and_wait(&crate::system::managed_background_unit(&manifest.id))
                    .await?;
            }
            None => {}
        }
        Ok(())
    }

    pub(super) async fn record_runtime_exit(
        &self,
        id: AppId,
        generation: u64,
        exit_code: i32,
        reported_crash: bool,
        source: ExitReportSource,
    ) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        let pending = PendingExit { generation, source };
        let accepted = {
            let mut reports = self.runtime_exit_reports.write().await;
            consume_exit_report(&mut reports, &id, pending)
        };
        if !accepted {
            match source {
                ExitReportSource::Synthetic => {
                    info!(%id, generation, "ignored synthetic exit superseded by runner report");
                }
                ExitReportSource::Runner => {
                    warn!(%id, generation, "ignored runner exit without a matching reservation");
                }
                ExitReportSource::Controlled => {
                    info!(%id, generation, "ignored completed controlled exit report");
                }
            }
            return Ok(());
        }
        self.runtime_missing_observations.write().await.remove(&id);
        if !self.take_matching_generation(&id, generation).await {
            warn!(%id, generation, exit_code, "ignored stale runtime-exit notification");
            return Ok(());
        }
        self.runtime_foreground_fences.write().await.remove(&id);
        self.runtime_background_execution.write().await.remove(&id);
        self.runtime_input_modes.write().await.remove(&id);
        let crashed = utils::runtime_exit_is_crash(exit_code, reported_crash);
        let domain = self.state.read().await.domain.clone();
        let was_foreground = matches!(&domain, DomainState::Foreground(current) if current == &id);
        if was_foreground {
            utils::set_foreground_marker(None)?
        }
        if owns_domain(&domain, &id) {
            self.state
                .write()
                .await
                .apply(if crashed {
                    Transition::AppCrashed(id.clone())
                } else {
                    Transition::AppExited(id.clone())
                })
                .map_err(|e| e.to_string())?;
        }
        if was_foreground {
            self.show_manager_surface().await?
        }
        self.update_session_after_exit(id, generation, exit_code, crashed)
            .await
    }

    async fn take_matching_generation(&self, id: &AppId, generation: u64) -> bool {
        let mut generations = self.runtime_generations.write().await;
        if !utils::runtime_generation_matches(&generations, id, generation) {
            return false;
        }
        generations.remove(id);
        true
    }

    async fn update_session_after_exit(
        &self,
        id: AppId,
        generation: u64,
        exit_code: i32,
        crashed: bool,
    ) -> Result<(), String> {
        if !crashed {
            self.session_store.remove(&id).map_err(|e| e.to_string())?;
            self.sessions.write().await.remove(&id);
            info!(%id, generation, exit_code, "managed application exited normally");
            return Ok(());
        }
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
            updated_at: utils::unix_now(),
            last_error: Some(format!("process exited with code {exit_code}")),
        })
        .await?;
        warn!(%id, generation, exit_code, "managed application crashed");
        Ok(())
    }
}

fn consume_exit_report(
    reports: &mut BTreeMap<AppId, PendingExit>,
    id: &AppId,
    event: PendingExit,
) -> bool {
    if reports.get(id) != Some(&event) {
        return false;
    }
    reports.remove(id);
    true
}

fn clear_closed_last_app(state: &mut remagic_core::ManagerState, id: &AppId) {
    if state.last_app.as_ref() == Some(id) {
        state.last_app = None;
    }
}

fn owns_domain(domain: &DomainState, id: &AppId) -> bool {
    matches!(domain,
        DomainState::Launching(current)
        | DomainState::Foreground(current)
        | DomainState::Parking(current) if current == id
    )
}

#[cfg(test)]
mod exit_report_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64};

    fn session(app_id: &AppId, subtitle: &str) -> AppSession {
        AppSession {
            schema: 1,
            app_id: app_id.clone(),
            status: SessionStatus::Parked,
            title: app_id.to_string(),
            subtitle: subtitle.into(),
            resume_payload: Some(serde_json::json!({ "page": 17 })),
            updated_at: 1,
            last_error: None,
        }
    }

    #[tokio::test]
    async fn stopped_resident_session_is_durably_demoted_from_background_to_parked() {
        let id = AppId::new("koreader").unwrap();
        let root = temporary_path("demote-background");
        let mut background = session(&id, "第 17 页");
        background.status = SessionStatus::Background;
        let daemon = testing_daemon(
            remagic_core::ManagerState::default(),
            BTreeMap::new(),
            BTreeMap::from([(id.clone(), background)]),
            root.clone(),
        );

        daemon.mark_session_process_stopped(&id).await.unwrap();

        assert_eq!(
            daemon.sessions.read().await[&id].status,
            SessionStatus::Parked
        );
        assert_eq!(
            remagic_core::SessionStore::new(&root).load_all().unwrap()[&id].status,
            SessionStatus::Parked
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    fn testing_daemon(
        state: remagic_core::ManagerState,
        manifests: BTreeMap<AppId, remagic_core::AppManifest>,
        sessions: BTreeMap<AppId, AppSession>,
        session_root: std::path::PathBuf,
    ) -> Daemon {
        let (events, _event_rx) = tokio::sync::mpsc::channel(1);
        let (power_control, _power_rx) = std::sync::mpsc::channel();
        Daemon {
            state: RwLock::new(state),
            manifests: RwLock::new(manifests),
            sessions: RwLock::new(sessions),
            runtime_generations: RwLock::new(BTreeMap::new()),
            runtime_background_execution: RwLock::new(BTreeMap::new()),
            runtime_foreground_fences: RwLock::new(BTreeMap::new()),
            runtime_input_modes: RwLock::new(BTreeMap::new()),
            runtime_exit_reports: RwLock::new(BTreeMap::new()),
            runtime_missing_observations: RwLock::new(BTreeMap::new()),
            session_store: remagic_core::SessionStore::new(session_root.clone()),
            manifest_store: remagic_core::ManifestStore::new(session_root.join("manifests")),
            controller: crate::system::SystemController::new(),
            power: Arc::new(crate::power_manager::PowerManager::load()),
            transition_lock: Mutex::new(()),
            events,
            power_control: power_device::ControlSender::from_test_channel(power_control),
            next_generation: AtomicU64::new(1),
            next_foreground_epoch: AtomicU64::new(1),
            next_sleep_epoch: AtomicU64::new(1),
            sleep_transaction: sleep::SleepTransaction::default(),
            launch_interrupt_epoch: Arc::new(AtomicU64::new(1)),
            cover_closed: Arc::new(AtomicBool::new(false)),
            cover_resume_app: RwLock::new(None),
            manager_repair_pending: AtomicBool::new(false),
            domain_recovery_pending: AtomicBool::new(false),
        }
    }

    fn temporary_path(label: &str) -> std::path::PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "remagic-park-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ))
    }

    #[test]
    fn runner_reservation_atomically_supersedes_synthetic_event() {
        let id = AppId::new("magicpaper").unwrap();
        let runner = PendingExit {
            generation: 7,
            source: ExitReportSource::Runner,
        };
        let synthetic = PendingExit {
            generation: 7,
            source: ExitReportSource::Synthetic,
        };
        let mut reports = BTreeMap::from([(id.clone(), synthetic)]);
        // This is the former TOCTOU window: a synthetic event is already
        // queued, then the runner publishes the authoritative normal exit.
        reports.insert(id.clone(), runner);
        assert!(!consume_exit_report(&mut reports, &id, synthetic));
        assert_eq!(reports.get(&id), Some(&runner));
        assert!(consume_exit_report(&mut reports, &id, runner));
        assert!(!reports.contains_key(&id));
    }

    #[test]
    fn explicit_close_removes_single_power_resume_target() {
        let id = AppId::new("koreader").unwrap();
        let mut state = remagic_core::ManagerState {
            domain: DomainState::Manager,
            last_app: Some(id.clone()),
            sequence: 9,
        };
        clear_closed_last_app(&mut state, &id);
        assert!(state.last_app.is_none());
    }

    #[tokio::test]
    async fn session_failure_leaves_domain_in_parking_for_fenced_recovery() {
        let id = AppId::new("magicpaper").unwrap();
        let old_session = session(&id, "old");
        let blocked_root = temporary_path("blocked-session-root");
        std::fs::write(&blocked_root, b"not a directory").unwrap();
        let daemon = testing_daemon(
            remagic_core::ManagerState {
                domain: DomainState::Foreground(id.clone()),
                last_app: Some(id.clone()),
                sequence: 4,
            },
            BTreeMap::new(),
            BTreeMap::from([(id.clone(), old_session.clone())]),
            blocked_root.clone(),
        );

        daemon.begin_park(&id, false).await.unwrap();
        assert!(daemon.save_session(session(&id, "new")).await.is_err());
        assert_eq!(
            daemon.state.read().await.domain,
            DomainState::Parking(id.clone())
        );
        assert_eq!(daemon.sessions.read().await.get(&id), Some(&old_session));

        daemon
            .state
            .write()
            .await
            .apply(Transition::AppRestored(id.clone()))
            .unwrap();
        assert_eq!(
            daemon.state.read().await.domain,
            DomainState::Foreground(id)
        );
        std::fs::remove_file(blocked_root).unwrap();
    }

    #[tokio::test]
    async fn late_v2_park_callback_is_ignored_after_foreground_recovery() {
        let id = AppId::new("magicpaper").unwrap();
        let manifest: remagic_core::AppManifest =
            toml::from_str(include_str!("../../../../manifests/magicpaper.toml")).unwrap();
        let root = temporary_path("late-ack");
        let daemon = testing_daemon(
            remagic_core::ManagerState {
                domain: DomainState::Foreground(id.clone()),
                last_app: Some(id.clone()),
                sequence: 7,
            },
            BTreeMap::from([(id.clone(), manifest)]),
            BTreeMap::new(),
            root.clone(),
        );

        daemon
            .record_parked(session(&id, "late old acknowledgement"))
            .await
            .unwrap();
        assert_eq!(
            daemon.state.read().await.domain,
            DomainState::Foreground(id)
        );
        assert!(daemon.sessions.read().await.is_empty());
        assert!(!root.exists());
    }
}
