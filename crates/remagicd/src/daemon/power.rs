use super::{Daemon, Event, QueuedEvent, HOME_EVENT_SOCKET};
use remagic_core::DomainState;
use std::sync::atomic::Ordering;
use tokio::net::UnixDatagram;
use tracing::info;

impl Daemon {
    pub(super) async fn handle_auto_sleep(
        &self,
        activity_revision: u64,
        interaction_epoch: u64,
    ) -> Result<(), String> {
        // Give raw input reporting one scheduler turn to overtake this
        // unattended transition. The check is bounded and occurs once per
        // idle cycle, never as a periodic timer.
        tokio::time::sleep(std::time::Duration::from_millis(75)).await;
        if self
            .launch_interrupt_epoch
            .load(std::sync::atomic::Ordering::Acquire)
            != interaction_epoch
            || !self.power.auto_sleep_is_current(activity_revision).await
        {
            return Ok(());
        }
        let domain = self.state.read().await.domain.clone();
        match domain {
            DomainState::Foreground(app) => self.park(app, false, true).await?,
            DomainState::Manager => self.ensure_manager_surface().await?,
            DomainState::System | DomainState::Sleeping => {
                self.power
                    .cancel_quiescing("automatic sleep no longer owns an awake managed domain")
                    .await;
                return Ok(());
            }
            _ => {
                self.power
                    .cancel_quiescing("automatic sleep yielded to a system transition")
                    .await;
                return Ok(());
            }
        }
        if !self.power.auto_sleep_is_current(activity_revision).await {
            return Ok(());
        }
        let socket = UnixDatagram::unbound()
            .map_err(|error| format!("cannot create Home event socket: {error}"))?;
        if let Err(error) = socket.send_to(b"auto_sleep\n", HOME_EVENT_SOCKET).await {
            self.power
                .cancel_quiescing("Home lock renderer unavailable")
                .await;
            return Err(format!("cannot request Home lock rendering: {error}"));
        }
        info!(
            activity_revision,
            "automatic sleep handed off to the lock renderer"
        );
        Ok(())
    }

    pub(super) async fn handle_cover_closed(&self, interrupt_epoch: u64) -> Result<(), String> {
        self.cover_closed.store(true, Ordering::Release);
        let domain = self.state.read().await.domain.clone();
        match domain {
            DomainState::System | DomainState::RestoringSystem => Ok(()),
            DomainState::Sleeping => Ok(()),
            DomainState::Manager => {
                *self.cover_resume_app.write().await = None;
                self.ensure_manager_surface().await?;
                if self.cover_closed.load(Ordering::Acquire) {
                    self.send_home_event("cover_sleep\n").await?;
                    info!("cover close handed off to Home lock renderer");
                }
                Ok(())
            }
            DomainState::Foreground(app) => {
                *self.cover_resume_app.write().await = Some(app.clone());
                self.park(app.clone(), false, true).await?;
                if self.cover_closed.load(Ordering::Acquire) {
                    self.send_home_event(&format!("cover_sleep_app {app}\n"))
                        .await?;
                    info!(%app, "cover close parked foreground app and handed off to Home");
                }
                Ok(())
            }
            DomainState::EnteringManaged
            | DomainState::Launching(_)
            | DomainState::Parking(_)
            | DomainState::Recovering => {
                self.schedule_cover_closed_retry(interrupt_epoch);
                Ok(())
            }
        }
    }

    pub(super) async fn handle_cover_opened(&self, _interrupt_epoch: u64) -> Result<(), String> {
        self.cover_closed.store(false, Ordering::Release);
        let domain = self.state.read().await.domain.clone();
        match domain {
            DomainState::Sleeping | DomainState::Manager => {
                let message = if let Some(app) = self.take_cover_resume_app().await {
                    format!("cover_open_app {app}\n")
                } else {
                    "cover_open\n".to_owned()
                };
                self.send_home_event(&message).await?;
                info!("cover open handed off to Home unlock renderer");
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn take_cover_resume_app(&self) -> Option<remagic_core::AppId> {
        self.cover_resume_app.write().await.take()
    }

    fn schedule_cover_closed_retry(&self, interaction_epoch: u64) {
        let events = self.events.clone();
        let launch_interrupt_epoch = self.launch_interrupt_epoch.clone();
        let cover_closed = self.cover_closed.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            if !cover_closed.load(Ordering::Acquire)
                || launch_interrupt_epoch.load(Ordering::Acquire) != interaction_epoch
            {
                return;
            }
            let _ = events
                .send(QueuedEvent::unattended(
                    Event::CoverClosed,
                    &launch_interrupt_epoch,
                ))
                .await;
        });
    }

    async fn send_home_event(&self, message: &str) -> Result<(), String> {
        let socket = UnixDatagram::unbound()
            .map_err(|error| format!("cannot create Home event socket: {error}"))?;
        socket
            .send_to(message.as_bytes(), HOME_EVENT_SOCKET)
            .await
            .map_err(|error| format!("cannot send Home event {message:?}: {error}"))?;
        Ok(())
    }
}
