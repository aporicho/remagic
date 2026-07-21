use super::*;
use crate::display_host;
use remagic_core::{DomainState, Transition};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing::warn;

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
        self.show_manager_surface(true).await?;
        self.state
            .write()
            .await
            .apply(Transition::ManagedReady)
            .map_err(|e| e.to_string())
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

    pub(super) async fn sleep(&self) -> Result<(), String> {
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
        display_host::full_refresh().await?;
        self.state
            .write()
            .await
            .apply(Transition::Wake)
            .map_err(|e| e.to_string())
    }

    pub(super) async fn restore_system(&self) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        if matches!(self.state.read().await.domain, DomainState::System) {
            return Ok(());
        }
        self.state.write().await.domain = DomainState::RestoringSystem;
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
        self.runtime_foreground_fences.write().await.clear();
        if let Err(error) = utils::set_foreground_marker(None) {
            warn!(%error, "could not clear foreground marker during system restore");
        }
        if let Err(error) = self.set_power_grab(false).await {
            warn!(%error, "could not confirm power-key release during system restore");
        }
        self.controller.restore_system().await?;
        self.state
            .write()
            .await
            .apply(Transition::SystemReady)
            .map_err(|e| e.to_string())
    }

    async fn stop_managed_apps(&self) -> Result<(), String> {
        let app_ids: Vec<_> = self.manifests.read().await.keys().cloned().collect();
        for app_id in app_ids {
            self.controller
                .stop_and_wait(&utils::app_unit(&app_id))
                .await?;
        }
        Ok(())
    }

    async fn set_power_grab(&self, grab: bool) -> Result<(), String> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.power_control
            .send(power_device::Control::Grab {
                grab,
                reply: reply_tx,
            })
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
        .await
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
}
