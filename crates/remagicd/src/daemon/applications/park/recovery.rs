use super::*;

impl Daemon {
    pub(super) async fn recover_failed_park(
        &self,
        id: &AppId,
        manifest: &remagic_core::AppManifest,
        fence: Option<ForegroundFence>,
        resume_payload: Option<serde_json::Value>,
    ) -> Result<String, String> {
        let restore = match fence {
            Some(fence) => {
                self.restore_foreground_after_failed_park(id, manifest, fence, resume_payload)
                    .await
            }
            None => Err("application does not support fenced lifecycle recovery".into()),
        };
        match restore {
            // A successfully persisted park snapshot remains the freshest
            // durable resume point even when the same process is foregrounded
            // again. If persistence itself failed, `save_session` left both
            // the on-disk and in-memory previous snapshots untouched.
            Ok(()) => Ok("application foreground was restored with a new lease".into()),
            Err(restore_error) => {
                self.force_stop_after_failed_park(id).await.map_err(|stop_error| {
                    format!(
                        "foreground restore failed ({restore_error}); forced stop failed ({stop_error})"
                    )
                })?;
                Ok(format!(
                    "foreground restore failed ({restore_error}); application was stopped and manager ownership was restored"
                ))
            }
        }
    }

    async fn restore_foreground_after_failed_park(
        &self,
        id: &AppId,
        manifest: &remagic_core::AppManifest,
        old_fence: ForegroundFence,
        resume_payload: Option<serde_json::Value>,
    ) -> Result<(), String> {
        self.validate_recovery_fence(id, old_fence).await?;
        let unit = utils::app_unit(id);
        if !self.controller.is_active_checked(&unit).await? {
            return Err(format!("application {id} exited during park recovery"));
        }
        let runtime_dir = manifest
            .runtime
            .directories
            .as_ref()
            .map(|directories| directories.runtime_dir.as_path())
            .ok_or_else(|| format!("application {id} has no runtime directory"))?;
        let foreground_epoch = self.allocate_foreground_epoch();
        let lease_id = foreground_epoch;
        app_runtime::command(
            runtime_dir,
            &AppCommand::EnterForeground {
                resume_payload,
                open_path: None,
                foreground_epoch: Some(foreground_epoch),
                lease_id: Some(lease_id),
            },
        )
        .await?;
        app_runtime::wait_event(
            runtime_dir,
            id,
            old_fence.generation,
            foreground_epoch,
            lease_id,
            "ready",
            Duration::from_millis(manifest.readiness.timeout_ms.max(100)),
        )
        .await?;
        if !self.controller.is_active_checked(&unit).await? {
            return Err(format!(
                "application {id} exited before foreground recovery commit"
            ));
        }
        self.commit_restored_foreground(
            id,
            manifest,
            old_fence.generation,
            foreground_epoch,
            lease_id,
        )
        .await
    }

    async fn validate_recovery_fence(
        &self,
        id: &AppId,
        old_fence: ForegroundFence,
    ) -> Result<(), String> {
        if !matches!(&self.state.read().await.domain, DomainState::Parking(current) if current == id)
        {
            return Err(format!("park recovery for {id} no longer owns Parking"));
        }
        if self.runtime_generations.read().await.get(id).copied() != Some(old_fence.generation)
            || self.runtime_foreground_fences.read().await.get(id).copied()
                != Some((old_fence.foreground_epoch, old_fence.lease_id))
        {
            return Err(format!(
                "park recovery for {id} lost its original runtime fence"
            ));
        }
        Ok(())
    }

    async fn commit_restored_foreground(
        &self,
        id: &AppId,
        manifest: &remagic_core::AppManifest,
        generation: u64,
        foreground_epoch: u64,
        lease_id: u64,
    ) -> Result<(), String> {
        let surface_key = display_host::app_surface_key(id);
        display_host::set_foreground(surface_key, generation, foreground_epoch, true).await?;
        display_host::configure_ink(
            surface_key,
            generation,
            foreground_epoch,
            manifest
                .capabilities
                .iter()
                .any(|capability| capability.as_str() == "ink:direct-v1"),
        )
        .await?;
        self.runtime_foreground_fences
            .write()
            .await
            .insert(id.clone(), (foreground_epoch, lease_id));
        self.state
            .write()
            .await
            .apply(Transition::AppRestored(id.clone()))
            .map_err(|error| error.to_string())?;
        utils::set_foreground_marker(Some(id))
    }

    async fn force_stop_after_failed_park(&self, id: &AppId) -> Result<(), String> {
        self.controller
            .stop_and_wait(&utils::app_unit(id))
            .await
            .map_err(|error| format!("could not stop {id}: {error}"))?;
        self.clear_runtime_tracking(id).await;
        {
            let mut state = self.state.write().await;
            match &state.domain {
                DomainState::Parking(current) | DomainState::Foreground(current)
                    if current == id =>
                {
                    state
                        .apply(Transition::AppExited(id.clone()))
                        .map_err(|error| error.to_string())?;
                }
                DomainState::Manager => {}
                current => {
                    return Err(format!(
                        "stopped {id} while manager domain was unexpectedly {current:?}"
                    ));
                }
            }
        }
        utils::set_foreground_marker(None)?;
        self.show_manager_surface(false).await
    }
}
