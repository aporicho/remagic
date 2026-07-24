use super::input_mode;
use super::*;
use crate::{app_runtime, display_host};
use remagic_core::{BackgroundService, DomainState, ReadinessMode, Transition};
use remagic_protocol::AppCommand;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing::warn;

mod background;
mod context;
mod generation;
mod rollback;
mod routing;
mod schema;

use background::should_quiesce_background;
use context::{ensure_foreground_capable, LaunchContext};
use routing::{launch_route, LaunchRoute};

impl Daemon {
    pub(super) async fn launch(
        &self,
        id: AppId,
        open_path: Option<PathBuf>,
        interrupt_epoch: u64,
        request_fence: &RequestFence,
    ) -> Result<(), String> {
        self.ensure_launch_current(interrupt_epoch, request_fence)?;
        let domain = self.state.read().await.domain.clone();
        match launch_route(&domain, &id, open_path.is_none())? {
            LaunchRoute::AlreadyForeground => return Ok(()),
            LaunchRoute::Park(current) => self.park(current, false, false).await?,
            LaunchRoute::Manager => {}
        }
        self.ensure_launch_current(interrupt_epoch, request_fence)?;
        let _guard = self.transition_lock.lock().await;
        if !matches!(self.state.read().await.domain, DomainState::Manager) {
            return Err("manager did not become ready for application launch".into());
        }
        let context = self.prepare_launch(id, open_path).await?;
        if let Err(error) = self
            .state
            .write()
            .await
            .apply(Transition::Launch(context.id.clone()))
        {
            let _ = self.start_background_service(&context).await;
            return Err(error.to_string());
        }

        let result = self
            .activate_and_wait(&context, interrupt_epoch, request_fence)
            .await;
        if let Err(error) = result {
            return Err(self.rollback_error(&context, error).await);
        }
        if let Err(error) = self
            .commit_launch(&context, interrupt_epoch, request_fence)
            .await
        {
            return Err(self.rollback_error(&context, error).await);
        }
        Ok(())
    }

    fn ensure_launch_current(
        &self,
        interrupt_epoch: u64,
        request_fence: &RequestFence,
    ) -> Result<(), String> {
        if request_fence.is_cancelled()
            || self.launch_interrupt_epoch.load(Ordering::Acquire) != interrupt_epoch
        {
            Err("application launch was superseded by a newer interaction".into())
        } else {
            Ok(())
        }
    }

    async fn commit_launch(
        &self,
        context: &LaunchContext,
        interrupt_epoch: u64,
        request_fence: &RequestFence,
    ) -> Result<(), String> {
        self.ensure_launch_current(interrupt_epoch, request_fence)?;
        if !request_fence.begin_commit() {
            return Err("application launch was cancelled before foreground commit".into());
        }
        if self.launch_interrupt_epoch.load(Ordering::Acquire) != interrupt_epoch {
            return Err("application launch was superseded at foreground commit".into());
        }
        // Keep the desired-mode record locked from the last read through panel
        // configuration and state publication. A concurrent startup request is
        // therefore either included in this commit or observes Foreground and
        // applies itself to the already-committed lease; it can never be ACKed
        // into the gap between those outcomes.
        let mut input_modes = self.runtime_input_modes.write().await;
        let input = input_modes
            .get_mut(&context.id)
            .ok_or_else(|| format!("application {} lost its pending input fence", context.id))?;
        let token = context.token();
        if !input.matches(&token) || !input.pending {
            return Err(format!(
                "application {} input fence changed before foreground commit",
                context.id
            ));
        }
        display_host::prepare_foreground(
            context.surface_key,
            context.generation,
            context.foreground_epoch,
        )
        .await?;
        display_host::activate_foreground(
            context.surface_key,
            context.generation,
            context.foreground_epoch,
            input.mode.ink_enabled(),
            true,
        )
        .await?;
        let mut state = self.state.write().await;
        state
            .apply(Transition::AppReady(context.id.clone()))
            .map_err(|error| error.to_string())?;
        input.pending = false;
        utils::set_foreground_marker(Some(&context.id))?;
        drop(state);
        drop(input_modes);
        self.power
            .set_presentation(remagic_core::PresentationState::Foreground(
                context.id.clone(),
            ))
            .await;
        Ok(())
    }

    async fn prepare_launch(
        &self,
        id: AppId,
        open_path: Option<PathBuf>,
    ) -> Result<LaunchContext, String> {
        let manifest = self
            .manifests
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or_else(|| format!("unknown application {id}"))?;
        ensure_foreground_capable(&manifest)?;
        if !manifest.exec.exists() {
            return Err(format!(
                "application executable is missing: {}",
                manifest.exec.display()
            ));
        }
        let open_path = open_path
            .map(|path| manifest.validate_open_path(&path))
            .transpose()
            .map_err(|e| e.to_string())?;
        let resume_payload = self
            .sessions
            .read()
            .await
            .get(&id)
            .and_then(|session| session.resume_payload.clone());
        let unit = utils::app_unit(&id);
        let active = self.validate_existing_runtime(&id, &unit).await?;
        let background_unit = match manifest.effective_background_service() {
            Some(BackgroundService::Systemd { unit }) => Some(unit),
            Some(BackgroundService::Managed { .. }) => {
                Some(crate::system::managed_background_unit(&manifest.id))
            }
            None => None,
        };
        let background_quiesced = should_quiesce_background(active, &manifest);
        if background_quiesced {
            if let Some(unit) = &background_unit {
                // The background agent writes the same trees protected by the
                // schema snapshot. Keep it stopped until the runner has
                // completed migration and published application readiness.
                self.controller.stop_and_wait(unit).await?;
            }
        } else {
            self.ensure_background_service(&manifest).await?;
        }
        let generation = if active {
            self.runtime_generations
                .read()
                .await
                .get(&id)
                .copied()
                .ok_or_else(|| format!("active application {id} lost its generation"))?
        } else {
            self.allocate_generation()
        };
        let captured_background_execution = self
            .runtime_background_execution
            .read()
            .await
            .get(&id)
            .copied();
        let background_execution = generation::background_execution(
            active,
            captured_background_execution,
            manifest.runtime.background_execution,
            &id,
        )?;
        let foreground_epoch = self.allocate_foreground_epoch();
        let runtime_dir = manifest
            .runtime
            .directories
            .as_ref()
            .map(|dirs| dirs.runtime_dir.clone())
            .ok_or_else(|| format!("application {id} has no runtime directory"))?;
        let launch_path = Path::new(RUNTIME_ROOT)
            .join("launch")
            .join(format!("{}.json", id.as_str()));
        Ok(LaunchContext {
            surface_key: display_host::app_surface_key(&id),
            lease_id: foreground_epoch,
            id,
            manifest,
            open_path,
            resume_payload,
            runtime_dir,
            unit,
            active,
            generation,
            background_execution,
            foreground_epoch,
            launch_path,
            background_quiesced,
        })
    }

    async fn validate_existing_runtime(&self, id: &AppId, unit: &str) -> Result<bool, String> {
        let mut active = self.controller.is_active_checked(unit).await?;
        let generation = self.runtime_generations.read().await.get(id).copied();
        let fence = self.runtime_foreground_fences.read().await.get(id).copied();
        let scheduling = self
            .runtime_background_execution
            .read()
            .await
            .get(id)
            .copied();
        if active && (generation.is_none() || fence.is_none() || scheduling.is_none()) {
            warn!(%id, "active application has no complete supervisor token; restarting safely");
            // The missing policy itself is why this process cannot be safely
            // recalled. Thaw unconditionally: it is idempotent for a running
            // unit and is required before stopping a possibly frozen one. A
            // crashed child can make the daemon return to Home just before
            // systemd finishes retiring the runner. In that narrow window the
            // unit may still report active while MainPID is already zero, so
            // thawing cannot be the terminal fence. Always continue to the
            // bounded stop; reaching inactive is the safety property needed
            // before a replacement generation starts.
            let thaw_error = self.controller.thaw_and_wait(unit).await.err();
            if let Err(stop_error) = self.controller.stop_and_wait(unit).await {
                return Err(match thaw_error {
                    Some(thaw_error) => format!(
                        "could not settle stale runtime {unit}: thaw failed: {thaw_error}; stop failed: {stop_error}"
                    ),
                    None => stop_error,
                });
            }
            if let Some(error) = thaw_error {
                warn!(%id, %error, "stale runtime settled after thaw raced process exit");
            }
            self.mark_session_process_stopped(id).await?;
            active = false;
            self.runtime_generations.write().await.remove(id);
            self.runtime_foreground_fences.write().await.remove(id);
            self.runtime_background_execution.write().await.remove(id);
        }
        Ok(active)
    }

    async fn activate_and_wait(
        &self,
        context: &LaunchContext,
        interrupt_epoch: u64,
        request_fence: &RequestFence,
    ) -> Result<(), String> {
        self.ensure_launch_current(interrupt_epoch, request_fence)?;
        self.publish_launch_descriptor(context)?;
        // Publish the runtime fence before starting or resuming the process so
        // its startup input-mode request cannot race registration.
        self.register_runtime(context).await?;
        if context.active {
            if context.background_execution.freezes_process() {
                self.controller
                    .thaw_and_wait(&context.unit)
                    .await
                    .map_err(|error| {
                        format!(
                            "could not thaw {} before foreground recall: {error}",
                            context.id
                        )
                    })?;
            }
            app_runtime::command(
                &context.runtime_dir,
                &AppCommand::EnterForeground {
                    resume_payload: context.resume_payload.clone(),
                    open_path: context.open_path.clone(),
                    foreground_epoch: Some(context.foreground_epoch),
                    lease_id: Some(context.lease_id),
                },
            )
            .await?;
        } else {
            schema::clear_phase_markers(context)?;
            self.controller.start(&context.unit).await?;
        }
        self.wait_for_application(context, interrupt_epoch, request_fence)
            .await?;
        self.ensure_launch_current(interrupt_epoch, request_fence)?;
        if !self.controller.is_active_checked(&context.unit).await? {
            return Err(format!("{} exited before foreground commit", context.id));
        }
        self.start_background_service(context).await?;
        self.ensure_launch_current(interrupt_epoch, request_fence)?;
        self.ensure_launch_current(interrupt_epoch, request_fence)
    }

    fn publish_launch_descriptor(&self, context: &LaunchContext) -> Result<(), String> {
        fs::create_dir_all(
            context
                .launch_path
                .parent()
                .expect("launch path has parent"),
        )
        .map_err(|e| e.to_string())?;
        if context.active {
            return fs::remove_file(&context.launch_path)
                .or_else(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        Ok(())
                    } else {
                        Err(error)
                    }
                })
                .map_err(|e| e.to_string());
        }
        utils::atomic_write_json(
            &context.launch_path,
            &serde_json::json!({
                "open_path": context.open_path,
                "resume_payload": context.resume_payload,
                "generation": context.generation,
                "foreground_epoch": context.foreground_epoch,
                "lease_id": context.lease_id,
                "qtfb_key": context.surface_key,
            }),
        )
    }

    async fn register_runtime(&self, context: &LaunchContext) -> Result<(), String> {
        self.runtime_generations
            .write()
            .await
            .insert(context.id.clone(), context.generation);
        self.runtime_background_execution
            .write()
            .await
            .insert(context.id.clone(), context.background_execution);
        self.runtime_foreground_fences.write().await.insert(
            context.id.clone(),
            (context.foreground_epoch, context.lease_id),
        );
        self.runtime_input_modes.write().await.insert(
            context.id.clone(),
            input_mode::RuntimeInputState::pending(&context.token(), &context.manifest)?,
        );
        Ok(())
    }

    async fn wait_for_application(
        &self,
        context: &LaunchContext,
        interrupt_epoch: u64,
        request_fence: &RequestFence,
    ) -> Result<(), String> {
        let (backup_timeout, migration_timeout, readiness_timeout, surface_timeout) =
            schema::startup_budgets(context);
        let readiness = async {
            if let Some(timeout) = backup_timeout {
                schema::wait_phase(context, remagic_core::SCHEMA_PREPARED_FILE, timeout).await?;
            }
            if let Some(timeout) = migration_timeout {
                schema::wait_phase(context, remagic_core::SCHEMA_COMPLETE_FILE, timeout).await?;
            }
            match context.manifest.readiness.mode {
                ReadinessMode::Lifecycle | ReadinessMode::FirstFrame => {
                    app_runtime::wait_event(
                        &context.runtime_dir,
                        &context.id,
                        context.generation,
                        context.foreground_epoch,
                        context.lease_id,
                        "ready",
                        readiness_timeout,
                    )
                    .await?;
                }
                ReadinessMode::File => {
                    let path = context.manifest.readiness.path.as_ref().ok_or_else(|| {
                        format!("application {} has no readiness file", context.id)
                    })?;
                    utils::wait_readiness_file(path, readiness_timeout).await?;
                }
                ReadinessMode::Process => {}
            }
            if context.manifest.display == "qtfb" {
                display_host::wait_surface(context.surface_key, surface_timeout).await?;
            }
            Ok(())
        };
        tokio::pin!(readiness);
        let failure = self.wait_for_runtime_failure(&context.unit);
        tokio::pin!(failure);
        let interrupted = self.wait_for_launch_interrupt(interrupt_epoch, request_fence);
        tokio::pin!(interrupted);
        tokio::select! {
            result = &mut readiness => result,
            error = &mut failure => Err(error),
            error = &mut interrupted => Err(error),
        }
    }

    async fn wait_for_launch_interrupt(
        &self,
        interrupt_epoch: u64,
        request_fence: &RequestFence,
    ) -> String {
        loop {
            if self
                .ensure_launch_current(interrupt_epoch, request_fence)
                .is_err()
            {
                return "application launch was cancelled by a newer interaction".into();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    async fn wait_for_runtime_failure(&self, unit: &str) -> String {
        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;
            match self.controller.is_active_checked(unit).await {
                Ok(true) => {}
                Ok(false) => return format!("{unit} exited before publishing readiness"),
                Err(error) => return format!("could not verify {unit} during launch: {error}"),
            }
            match self.controller.is_active_checked(DISPLAY_UNIT).await {
                Ok(true) => {}
                Ok(false) => return "display host exited while application was starting".into(),
                Err(error) => {
                    return format!("could not verify display host during launch: {error}");
                }
            }
        }
    }
}
