use super::{Daemon, HOME_EVENT_SOCKET};
use remagic_core::DomainState;
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
        match self.state.read().await.domain.clone() {
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
}
