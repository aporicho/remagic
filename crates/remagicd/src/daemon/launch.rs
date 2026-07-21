use super::*;
use crate::{app_runtime, display_host};
use remagic_core::{BackgroundService, DomainState, ReadinessMode, Transition};
use remagic_protocol::AppCommand;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing::warn;

mod schema;

struct LaunchContext {
    id: AppId,
    manifest: remagic_core::AppManifest,
    open_path: Option<PathBuf>,
    resume_payload: Option<serde_json::Value>,
    runtime_dir: PathBuf,
    unit: String,
    active: bool,
    generation: u64,
    foreground_epoch: u64,
    lease_id: u64,
    surface_key: i32,
    launch_path: PathBuf,
    background_unit: Option<String>,
    background_quiesced: bool,
}

#[derive(Debug, Eq, PartialEq)]
enum LaunchRoute {
    AlreadyForeground,
    Park(AppId),
    Manager,
}

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
        let mut state = self.state.write().await;
        self.ensure_launch_current(interrupt_epoch, request_fence)?;
        if !request_fence.begin_commit() {
            return Err("application launch was cancelled before foreground commit".into());
        }
        if self.launch_interrupt_epoch.load(Ordering::Acquire) != interrupt_epoch {
            return Err("application launch was superseded at foreground commit".into());
        }
        state
            .apply(Transition::AppReady(context.id.clone()))
            .map_err(|error| error.to_string())?;
        utils::set_foreground_marker(Some(&context.id))
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
            Some(BackgroundService::Managed { .. }) | None => None,
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
            foreground_epoch,
            launch_path,
            background_unit,
            background_quiesced,
        })
    }

    async fn ensure_background_service(
        &self,
        manifest: &remagic_core::AppManifest,
    ) -> Result<(), String> {
        if let Some(BackgroundService::Systemd { unit }) = manifest.effective_background_service() {
            if !self.controller.is_active_checked(&unit).await? {
                self.controller.start_and_wait(&unit).await?
            }
        }
        Ok(())
    }

    async fn validate_existing_runtime(&self, id: &AppId, unit: &str) -> Result<bool, String> {
        let mut active = self.controller.is_active_checked(unit).await?;
        let generation = self.runtime_generations.read().await.get(id).copied();
        let fence = self.runtime_foreground_fences.read().await.get(id).copied();
        if active && (generation.is_none() || fence.is_none()) {
            warn!(%id, "active application has no complete supervisor token; restarting safely");
            self.controller.stop_and_wait(unit).await?;
            active = false;
            self.runtime_generations.write().await.remove(id);
            self.runtime_foreground_fences.write().await.remove(id);
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
        if context.active {
            self.register_runtime(context).await;
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
            self.register_runtime(context).await;
        }
        self.wait_for_application(context, interrupt_epoch, request_fence)
            .await?;
        self.ensure_launch_current(interrupt_epoch, request_fence)?;
        if !self.controller.is_active_checked(&context.unit).await? {
            return Err(format!("{} exited before foreground commit", context.id));
        }
        self.start_background_service(context).await?;
        self.ensure_launch_current(interrupt_epoch, request_fence)?;
        display_host::set_foreground(
            context.surface_key,
            context.generation,
            context.foreground_epoch,
            true,
        )
        .await?;
        self.ensure_launch_current(interrupt_epoch, request_fence)?;
        display_host::configure_ink(
            context.surface_key,
            context.generation,
            context.foreground_epoch,
            context
                .manifest
                .capabilities
                .iter()
                .any(|cap| cap.as_str() == "ink:direct-v1"),
        )
        .await?;
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

    async fn register_runtime(&self, context: &LaunchContext) {
        self.runtime_generations
            .write()
            .await
            .insert(context.id.clone(), context.generation);
        self.runtime_foreground_fences.write().await.insert(
            context.id.clone(),
            (context.foreground_epoch, context.lease_id),
        );
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

    async fn start_background_service(&self, context: &LaunchContext) -> Result<(), String> {
        if let Some(unit) = &context.background_unit {
            if !self.controller.is_active_checked(unit).await? {
                self.controller.start_and_wait(unit).await?;
            }
        }
        Ok(())
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

    async fn rollback_error(&self, context: &LaunchContext, cause: String) -> String {
        match self.rollback_launch(context).await {
            Ok(()) => format!("{cause}; {} launch was rolled back", context.id),
            Err(rollback) => format!(
                "{cause}; {} rollback failed and requires domain recovery: {rollback}",
                context.id
            ),
        }
    }

    async fn rollback_launch(&self, context: &LaunchContext) -> Result<(), String> {
        // Nothing below this fence may erase ownership evidence or claim that
        // Home is authoritative while the failed application cgroup survives.
        self.controller
            .stop_and_wait(&context.unit)
            .await
            .map_err(|error| format!("could not stop {}: {error}", context.unit))?;
        let _ = fs::remove_file(&context.launch_path);
        self.runtime_generations.write().await.remove(&context.id);
        self.runtime_foreground_fences
            .write()
            .await
            .remove(&context.id);
        self.runtime_exit_reports.write().await.remove(&context.id);
        self.runtime_missing_observations
            .write()
            .await
            .remove(&context.id);
        self.state
            .write()
            .await
            .apply(Transition::AppExited(context.id.clone()))
            .map_err(|error| format!("could not roll back launch state: {error}"))?;
        self.show_manager_surface(false)
            .await
            .map_err(|error| format!("could not restore manager surface: {error}"))?;
        let schema_safe = schema::background_restore_is_safe(context);
        if schema_safe {
            self.start_background_service(context)
                .await
                .map_err(|error| format!("could not restore background service: {error}"))?;
        } else {
            warn!(
                app = %context.id,
                generation = context.generation,
                "background service remains stopped because schema completion was not proven"
            );
        }
        Ok(())
    }
}

fn should_quiesce_background(active: bool, manifest: &remagic_core::AppManifest) -> bool {
    !active && manifest.data_schema.is_some()
}

fn launch_route(domain: &DomainState, id: &AppId, no_path: bool) -> Result<LaunchRoute, String> {
    match domain {
        DomainState::Foreground(current) if current == id && no_path => {
            Ok(LaunchRoute::AlreadyForeground)
        }
        DomainState::Foreground(current) => Ok(LaunchRoute::Park(current.clone())),
        DomainState::Manager => Ok(LaunchRoute::Manager),
        _ => Err("applications can only be launched from manager or foreground app".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relaunch_of_current_app_without_path_is_idempotent() {
        let app = AppId::new("magicpaper").unwrap();
        assert_eq!(
            launch_route(&DomainState::Foreground(app.clone()), &app, true).unwrap(),
            LaunchRoute::AlreadyForeground,
        );
        assert_eq!(
            launch_route(&DomainState::Foreground(app.clone()), &app, false).unwrap(),
            LaunchRoute::Park(app),
        );
    }

    #[test]
    fn only_a_cold_schema_launch_quiesces_its_background_writer() {
        let mut manifest: remagic_core::AppManifest =
            toml::from_str(include_str!("../../../../manifests/magicpaper.toml")).unwrap();
        assert!(should_quiesce_background(false, &manifest));
        assert!(!should_quiesce_background(true, &manifest));
        manifest.data_schema = None;
        assert!(!should_quiesce_background(false, &manifest));
    }
}
