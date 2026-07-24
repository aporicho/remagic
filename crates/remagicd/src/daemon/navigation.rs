use super::*;
use crate::display_host;
use remagic_core::{DomainState, PresentationState, Transition};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing::warn;

mod suspend;

impl Daemon {
    pub(super) async fn single_power(
        &self,
        interrupt_epoch: u64,
        request_fence: &RequestFence,
    ) -> Result<(), String> {
        let domain = self.state.read().await.domain.clone();
        match domain {
            DomainState::System => Ok(()),
            DomainState::Manager => {
                let last = self.state.read().await.last_app.clone();
                if let Some(app) = last {
                    self.launch(app, None, interrupt_epoch, request_fence).await
                } else {
                    Ok(())
                }
            }
            DomainState::Foreground(app) => self.park(app, false, true).await,
            // The physical wake press is consumed by the wake guard. A later
            // deliberate single press while the lock page is awake puts the
            // same frozen lock transaction back to sleep.
            DomainState::Sleeping => {
                let sleep = self.sleep_transaction.snapshot();
                self.resuspend_locked(sleep.epoch, sleep.revision, interrupt_epoch)
                    .await
            }
            _ => Ok(()),
        }
    }

    pub(super) async fn triple_power(&self) -> Result<(), String> {
        let domain = self.state.read().await.domain.clone();
        match domain {
            DomainState::System => self.enter_manager().await,
            DomainState::Foreground(app) => {
                self.park(app, true, false).await?;
                self.restore_system().await
            }
            DomainState::Manager => self.restore_system().await,
            DomainState::Sleeping => self.restore_system().await,
            _ => Ok(()),
        }
    }

    async fn enter_manager(&self) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        self.state
            .write()
            .await
            .apply(Transition::TriplePower)
            .map_err(|e| e.to_string())?;
        self.set_power_grab(true).await?;
        self.controller.enter_managed().await?;
        utils::set_foreground_marker(None)?;
        self.controller.start(DISPLAY_UNIT).await?;
        display_host::wait_ready().await?;
        self.controller.start(HOME_UNIT).await?;
        display_host::wait_surface(display_host::HOME_SURFACE_KEY, Duration::from_secs(8)).await?;
        self.show_manager_surface(false).await?;
        self.state
            .write()
            .await
            .apply(Transition::ManagedReady)
            .map_err(|e| e.to_string())?;
        self.power.enter_managed().await;
        Ok(())
    }

    pub(super) async fn open_manager(&self) -> Result<(), String> {
        let domain = self.state.read().await.domain.clone();
        match domain {
            DomainState::System => self.enter_manager().await,
            DomainState::Foreground(app) => self.park(app, false, true).await,
            DomainState::Manager => self.ensure_manager_surface().await,
            _ => Ok(()),
        }
    }

    pub(super) async fn restore_system(&self) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        if matches!(self.state.read().await.domain, DomainState::System) {
            return Ok(());
        }
        self.state.write().await.domain = DomainState::RestoringSystem;
        self.sleep_transaction.reset()?;
        if let Err(error) = self.cancel_wake_guard().await {
            warn!(%error, "could not cancel wake guard during system restore");
        }
        let _ = display_host::clear_foreground().await;
        self.controller.stop_and_wait(HOME_UNIT).await?;
        self.stop_managed_apps().await?;
        self.controller
            .stop_and_wait("remagic-app@*.service")
            .await?;
        self.controller
            .stop_and_wait("remagic-runtime.service")
            .await?;
        self.controller.stop_and_wait(DISPLAY_UNIT).await?;
        self.runtime_generations.write().await.clear();
        self.runtime_background_execution.write().await.clear();
        self.runtime_foreground_fences.write().await.clear();
        self.runtime_input_modes.write().await.clear();
        if let Err(error) = utils::set_foreground_marker(None) {
            warn!(%error, "could not clear foreground marker during system restore");
        }
        self.controller.restore_system().await?;
        // Keep the power key grabbed until xochitl is active. Releasing it
        // earlier creates an interval in which no foreground owner can handle
        // the user's key gesture.
        self.set_power_grab(false).await?;
        self.state
            .write()
            .await
            .apply(Transition::SystemReady)
            .map_err(|e| e.to_string())?;
        self.power.enter_stock().await;
        Ok(())
    }

    async fn stop_managed_apps(&self) -> Result<(), String> {
        let apps: Vec<_> = self.manifests.read().await.values().cloned().collect();
        for manifest in apps {
            let app_id = manifest.id;
            let unit = utils::app_unit(&app_id);
            if self
                .runtime_background_execution
                .read()
                .await
                .get(&app_id)
                .copied()
                .is_some_and(remagic_core::BackgroundExecution::freezes_process)
                && self.controller.is_active_checked(&unit).await?
            {
                self.controller
                    .thaw_and_wait(&unit)
                    .await
                    .map_err(|error| {
                        format!("could not thaw {app_id} before system restore: {error}")
                    })?;
            }
            self.controller.stop_and_wait(&unit).await?;
            if let Err(error) = self.mark_session_process_stopped(&app_id).await {
                // Restoring the stock display owner is a safety boundary and
                // must not be blocked by task-list metadata persistence.
                warn!(%app_id, %error, "could not mark stopped application session as parked");
            }
        }
        Ok(())
    }

    async fn set_power_grab(&self, grab: bool) -> Result<(), String> {
        self.send_power_control(|reply| power_device::Control::Grab { grab, reply })
            .await
    }

    async fn arm_wake_guard(&self) -> Result<(), String> {
        self.send_power_control(|reply| power_device::Control::ArmWakeGuard { reply })
            .await
    }

    async fn resume_wake_guard(&self) -> Result<(), String> {
        self.send_power_control(|reply| power_device::Control::ResumeWakeGuard { reply })
            .await
    }

    async fn cancel_wake_guard(&self) -> Result<(), String> {
        self.send_power_control(|reply| power_device::Control::CancelWakeGuard { reply })
            .await
    }

    async fn send_power_control(
        &self,
        command: impl FnOnce(tokio::sync::oneshot::Sender<Result<(), String>>) -> power_device::Control,
    ) -> Result<(), String> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.power_control
            .send(command(reply_tx))
            .map_err(|error| format!("power input thread is unavailable: {error}"))?;
        tokio::time::timeout(Duration::from_secs(1), reply_rx)
            .await
            .map_err(|_| "power input grab acknowledgement timed out".to_string())?
            .map_err(|_| "power input thread closed without acknowledgement".to_string())?
    }

    pub(super) async fn restart_runtime_and_wait(&self) -> Result<(), String> {
        self.controller.restart(HOME_UNIT).await?;
        display_host::wait_surface(display_host::HOME_SURFACE_KEY, Duration::from_secs(8)).await?;
        self.show_manager_surface(false).await
    }

    pub(super) async fn show_manager_surface(&self, full_refresh: bool) -> Result<(), String> {
        display_host::wait_surface(display_host::HOME_SURFACE_KEY, Duration::from_secs(8)).await?;
        display_host::set_foreground(
            display_host::HOME_SURFACE_KEY,
            self.allocate_generation(),
            self.allocate_foreground_epoch(),
            full_refresh,
        )
        .await?;
        self.power.set_presentation(PresentationState::Home).await;
        Ok(())
    }

    pub(super) async fn ensure_manager_surface(&self) -> Result<(), String> {
        if !matches!(self.state.read().await.domain, DomainState::Manager) {
            return Ok(());
        }
        if !self.controller.is_active_checked(DISPLAY_UNIT).await? {
            return Err("managed display host stopped while manager owned the domain".into());
        }
        if !self.controller.is_active_checked(HOME_UNIT).await? {
            self.controller.start(HOME_UNIT).await?
        }
        display_host::wait_surface(display_host::HOME_SURFACE_KEY, Duration::from_secs(8)).await?;
        if display_host::status()
            .await
            .is_ok_and(|snapshot| snapshot.foreground_key == Some(display_host::HOME_SURFACE_KEY))
        {
            return Ok(());
        }
        self.show_manager_surface(false).await
    }

    pub(super) async fn ensure_manager_or_restore(&self) -> Result<(), String> {
        match self.ensure_manager_surface().await {
            Ok(()) => Ok(()),
            Err(display_error) => {
                warn!(%display_error, "manager surface unavailable after request failure");
                self.restore_system().await
            }
        }
    }

    pub(super) fn allocate_generation(&self) -> u64 {
        self.next_generation.fetch_add(1, Ordering::Relaxed).max(1)
    }

    pub(super) fn allocate_foreground_epoch(&self) -> u64 {
        self.next_foreground_epoch
            .fetch_add(1, Ordering::Relaxed)
            .max(1)
    }

    pub(super) fn allocate_sleep_epoch(&self) -> u64 {
        self.next_sleep_epoch.fetch_add(1, Ordering::Relaxed).max(1)
    }
}
