use super::super::{sleep, Daemon, Event, QueuedEvent, RequestFence};
use crate::display_host;
use remagic_core::{DomainState, Transition};
use sleep::SleepPhase;
use std::fs;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tracing::{info, warn};

const LOCK_AWAKE_TIMEOUT: Duration = Duration::from_secs(30);

impl Daemon {
    pub(in crate::daemon) async fn sleep(
        &self,
        lock_surface_sequence: u64,
        request_fence: &RequestFence,
    ) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        let (sleep_epoch, interaction_epoch) = self
            .prepare_sleep_lock(lock_surface_sequence, request_fence)
            .await?;
        if let Err(error) = self.arm_wake_guard().await {
            return self.rollback_sleep(sleep_epoch, error).await;
        }
        if self.launch_interrupt_epoch.load(Ordering::Acquire) != interaction_epoch {
            return self
                .rollback_sleep(
                    sleep_epoch,
                    "sleep cancelled by newer user interaction".into(),
                )
                .await;
        }
        self.power.suspended().await;
        let suspend_result = self.controller.suspend().await;
        // The managed owner must be awake-locked again before any display or
        // input recovery work is attempted.
        let wakelock_result = self.controller.acquire_wakelock();

        if let Err(error) = suspend_result {
            self.power.externally_blocked(&error).await;
            let guard_result = self.cancel_wake_guard().await;
            if let Err(wakelock_error) = wakelock_result {
                return Err(format!("{error}; wake-lock recovery: {wakelock_error}"));
            }
            if let Err(guard_error) = guard_result {
                return Err(format!("{error}; wake-key recovery: {guard_error}"));
            }
            self.schedule_blocked_resuspend(self.sleep_transaction.snapshot(), interaction_epoch);
            // The lock page and input fence were already committed. A power
            // inhibitor is not permission to expose Home again: retain the
            // retryable Sleeping domain. A later single power press asks Home
            // to render the replacement manager frame and perform the normal
            // fenced unlock, even while the charger remains attached.
            return Err(sleep::retained_lock_error(&error));
        }
        let guard_result = self.resume_wake_guard().await;
        wakelock_result?;
        guard_result?;
        self.finish_physical_resume(sleep_epoch).await
    }

    async fn prepare_sleep_lock(
        &self,
        lock_surface_sequence: u64,
        request_fence: &RequestFence,
    ) -> Result<(u64, u64), String> {
        if !matches!(self.state.read().await.domain, DomainState::Manager) {
            return Err("sleep button is only available in the manager".into());
        }
        if !request_fence.begin_commit() {
            return Err("sleep request was cancelled before commit".into());
        }
        let interaction_epoch = self.launch_interrupt_epoch.load(Ordering::Acquire);
        if lock_surface_sequence == 0 {
            return Err("sleep requires a committed ReMagic lock page".into());
        }
        let lock_surface = display_host::wait_surface_sequence(
            display_host::HOME_SURFACE_KEY,
            lock_surface_sequence,
            Duration::from_secs(2),
        )
        .await?;
        if lock_surface.foreground_key != Some(display_host::HOME_SURFACE_KEY) {
            return Err("manager lock page is not the visible surface".into());
        }
        let sleep_epoch = self.allocate_sleep_epoch();
        self.sleep_transaction.begin(sleep_epoch)?;
        if let Err(error) = self.state.write().await.apply(Transition::Sleep) {
            let reset_error = self.sleep_transaction.reset().err();
            let mut message = error.to_string();
            if let Some(reset_error) = reset_error {
                message.push_str(&format!("; sleep transaction reset failed: {reset_error}"));
            }
            return Err(message);
        }
        let generation = self.allocate_generation();
        let foreground_epoch = self.allocate_foreground_epoch();
        if let Err(error) = display_host::show_lock(
            display_host::HOME_SURFACE_KEY,
            generation,
            foreground_epoch,
            sleep_epoch,
        )
        .await
        {
            return self.rollback_sleep(sleep_epoch, error).await;
        }
        if let Err(error) = self.sleep_transaction.mark_locked(sleep_epoch) {
            return self.rollback_sleep(sleep_epoch, error).await;
        }
        self.backlight.force_off("lock");
        self.power.begin_suspend().await;
        Ok((sleep_epoch, interaction_epoch))
    }

    /// Re-enter suspend without exposing Home or changing the frozen lock
    /// lease. Timers carry both the sleep revision and the user-interaction
    /// epoch, so a queued unlock or newer power action always wins.
    pub(in crate::daemon) async fn resuspend_locked(
        &self,
        sleep_epoch: u64,
        sleep_revision: u64,
        interaction_epoch: u64,
    ) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        if !matches!(self.state.read().await.domain, DomainState::Sleeping) {
            return Ok(());
        }
        let current_interaction = self.launch_interrupt_epoch.load(Ordering::Acquire);
        if !sleep::resuspend_fence_matches(
            self.sleep_transaction.snapshot(),
            sleep_epoch,
            sleep_revision,
            current_interaction,
            interaction_epoch,
        ) {
            return Ok(());
        }

        info!(
            sleep_epoch,
            sleep_revision, "locked display is re-entering suspend"
        );
        self.arm_wake_guard_for_locked_resuspend().await?;
        // Wake requests are accepted on independent control tasks. Recheck
        // after the asynchronous power-thread handshake so a touch that
        // arrived during it can cancel this low-priority automatic action.
        if self.launch_interrupt_epoch.load(Ordering::Acquire) != interaction_epoch {
            self.cancel_wake_guard().await?;
            return Ok(());
        }
        self.backlight.force_off("locked resuspend");
        self.power.suspended().await;
        let suspend_result = self.controller.suspend().await;
        let wakelock_result = self.controller.acquire_wakelock();
        if let Err(suspend_error) = suspend_result {
            self.power.externally_blocked(&suspend_error).await;
            let guard_result = self.cancel_wake_guard().await;
            if wakelock_result.is_ok() && guard_result.is_ok() {
                self.schedule_blocked_resuspend(
                    self.sleep_transaction.snapshot(),
                    interaction_epoch,
                );
                return Err(sleep::retained_lock_error(&suspend_error));
            }
            let mut failures = vec![format!("suspend: {suspend_error}")];
            for (stage, result) in [
                ("wake-lock recovery", wakelock_result),
                ("wake-key recovery", guard_result),
            ] {
                if let Err(error) = result {
                    failures.push(format!("{stage}: {error}"));
                }
            }
            return Err(format!(
                "locked resuspend transaction failed: {}",
                failures.join("; ")
            ));
        }
        let guard_result = self.resume_wake_guard().await;
        let mut failures = Vec::new();
        for (stage, result) in [
            ("wake-lock recovery", wakelock_result),
            ("wake-key recovery", guard_result),
        ] {
            if let Err(error) = result {
                failures.push(format!("{stage}: {error}"));
            }
        }
        if !failures.is_empty() {
            return Err(format!(
                "locked resuspend transaction failed: {}",
                failures.join("; ")
            ));
        }
        self.finish_physical_resume(sleep_epoch).await
    }

    async fn arm_wake_guard_for_locked_resuspend(&self) -> Result<(), String> {
        match self.arm_wake_guard().await {
            Ok(()) => Ok(()),
            Err(error) if wake_guard_is_already_armed(&error) => {
                warn!(%error, "clearing stale wake guard before locked resuspend");
                self.cancel_wake_guard().await?;
                self.arm_wake_guard().await
            }
            Err(error) => Err(error),
        }
    }

    async fn finish_physical_resume(&self, sleep_epoch: u64) -> Result<(), String> {
        // The e-paper panel already retains the committed lock image. Do not
        // repaint it after resume: Home will prepare the manager surface and
        // replace the lock with one full transaction as soon as this request
        // returns. Keep the timer only as a fail-safe if Home cannot do so.
        let display = display_host::status().await?;
        if display.lock_epoch != sleep_epoch
            || !display.lock_committed
            || display.foreground_key != Some(display_host::HOME_SURFACE_KEY)
        {
            return Err(format!(
                "display lock epoch {sleep_epoch} was not retained across suspend"
            ));
        }
        let awake = self.sleep_transaction.mark_awake(sleep_epoch)?;
        self.schedule_locked_resuspend(awake);
        if self.cover_closed.load(Ordering::Acquire) {
            info!("physical resume retained the lock because the cover is closed");
            return Ok(());
        }
        if let Err(error) = self.request_home_unlock().await {
            // The panel remains safely locked. Notification loss is
            // recoverable UI state, so the next single press may retry it.
            warn!(%error, "Home resume notification was lost");
        }
        Ok(())
    }

    pub(in crate::daemon) async fn request_home_unlock(&self) -> Result<(), String> {
        let socket = tokio::net::UnixDatagram::unbound()
            .map_err(|error| format!("cannot create Home resume socket: {error}"))?;
        socket
            .send_to(b"resume_unlock\n", super::super::HOME_EVENT_SOCKET)
            .await
            .map_err(|error| format!("cannot notify Home to unlock: {error}"))?;
        Ok(())
    }

    fn schedule_locked_resuspend(&self, sleep: sleep::SleepSnapshot) {
        let events = self.events.clone();
        let launch_interrupt_epoch = self.launch_interrupt_epoch.clone();
        let interaction_epoch = launch_interrupt_epoch.load(Ordering::Acquire);
        tokio::spawn(async move {
            tokio::time::sleep(LOCK_AWAKE_TIMEOUT).await;
            if launch_interrupt_epoch.load(Ordering::Acquire) != interaction_epoch {
                return;
            }
            let _ = events
                .send(QueuedEvent::unattended(
                    Event::Resuspend {
                        sleep_epoch: sleep.epoch,
                        sleep_revision: sleep.revision,
                        interaction_epoch,
                    },
                    &launch_interrupt_epoch,
                ))
                .await;
        });
    }

    /// A charger wake lock is expected while USB power is attached. Keep the
    /// lock page committed and retry only after every external blocker has
    /// disappeared. The interaction epoch cancels this observer immediately
    /// when the user unlocks, restores stock, or starts another transition.
    fn schedule_blocked_resuspend(&self, sleep: sleep::SleepSnapshot, interaction_epoch: u64) {
        let events = self.events.clone();
        let launch_interrupt_epoch = self.launch_interrupt_epoch.clone();
        tokio::spawn(async move {
            loop {
                if launch_interrupt_epoch.load(Ordering::Acquire) != interaction_epoch {
                    return;
                }
                if external_wake_locks_are_clear() {
                    let _ = events
                        .send(QueuedEvent::unattended(
                            Event::Resuspend {
                                sleep_epoch: sleep.epoch,
                                sleep_revision: sleep.revision,
                                interaction_epoch,
                            },
                            &launch_interrupt_epoch,
                        ))
                        .await;
                    return;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }

    pub(in crate::daemon) async fn wake(
        &self,
        manager_surface_sequence: u64,
        request_fence: &RequestFence,
    ) -> Result<(), String> {
        let _guard = self.transition_lock.lock().await;
        let domain = self.state.read().await.domain.clone();
        if matches!(domain, DomainState::Manager) {
            return Ok(());
        }
        if !matches!(domain, DomainState::Sleeping) {
            return Err("ReMagic is not locked".into());
        }
        if !request_fence.begin_commit() {
            return Err("wake request was cancelled before commit".into());
        }
        if manager_surface_sequence == 0 {
            return Err("wake requires a committed ReMagic manager page".into());
        }
        let replacement = display_host::wait_surface_sequence(
            display_host::HOME_SURFACE_KEY,
            manager_surface_sequence,
            Duration::from_secs(2),
        )
        .await?;
        if replacement.foreground_key != Some(display_host::HOME_SURFACE_KEY) {
            return Err("manager replacement page does not own the locked surface".into());
        }
        let sleep = self.sleep_transaction.snapshot();
        if sleep.epoch == 0 || sleep.phase != SleepPhase::Locked {
            return Err(format!(
                "sleep state has no committed lock transaction ({:?}, epoch {})",
                sleep.phase, sleep.epoch
            ));
        }
        let sleep_epoch = sleep.epoch;
        // A previous post-resume acquisition may have failed while correctly
        // leaving the panel locked. Re-prove the managed wakelock before any
        // retry can expose the manager and its applications.
        self.controller.acquire_wakelock()?;
        // Fence supervision before display input is reopened. Any recovery
        // event produced from the old Locked snapshot is stale after this.
        self.sleep_transaction.begin_unlock(sleep_epoch)?;
        if let Err(error) = self.cancel_wake_guard().await {
            return self.abort_wake(sleep_epoch, error);
        }
        if let Err(error) = display_host::cancel_lock(sleep_epoch, manager_surface_sequence).await {
            return self.abort_wake(sleep_epoch, error);
        }
        self.state
            .write()
            .await
            .apply(Transition::Wake)
            .map_err(|error| error.to_string())?;
        self.sleep_transaction.finish_unlock(sleep_epoch)?;
        self.power.resumed("power key").await;
        self.backlight.restore_desired();
        Ok(())
    }

    fn abort_wake<T>(&self, sleep_epoch: u64, cause: String) -> Result<T, String> {
        match self.sleep_transaction.abort_unlock(sleep_epoch) {
            Ok(_) => Err(cause),
            Err(rollback_error) => Err(format!(
                "{cause}; unlock transaction rollback failed: {rollback_error}"
            )),
        }
    }

    async fn rollback_sleep<T>(&self, sleep_epoch: u64, cause: String) -> Result<T, String> {
        let sleep = self.sleep_transaction.snapshot();
        if sleep.epoch == sleep_epoch
            && matches!(sleep.phase, SleepPhase::Preparing | SleepPhase::Locked)
        {
            if let Err(error) = self.sleep_transaction.begin_unlock(sleep_epoch) {
                return Err(format!(
                    "{cause}; sleep rollback could not begin unlock: {error}"
                ));
            }
        }
        // Never release the display/input fence when the power-key fence could
        // not be cancelled. Keeping both the domain and panel locked is the
        // only retryable fail-closed result.
        if let Err(error) = self.cancel_wake_guard().await {
            return self.abort_wake(
                sleep_epoch,
                format!("{cause}; wake-guard rollback failed: {error}"),
            );
        }
        let display_result = match display_host::status().await {
            Ok(snapshot) if snapshot.lock_epoch == 0 => Ok(()),
            Ok(snapshot) if snapshot.lock_epoch == sleep_epoch => {
                // Rollback has no independently proven replacement frame. It
                // is allowed only while the original manager surface remains
                // the authoritative client buffer; its latest commit is used
                // as the atomic replacement before input is reopened.
                let replacement = snapshot
                    .surface_sequences
                    .get(&display_host::HOME_SURFACE_KEY)
                    .copied()
                    .unwrap_or(0);
                if replacement == 0 {
                    Err("display lock rollback has no committed manager surface".into())
                } else {
                    display_host::cancel_lock(sleep_epoch, replacement).await
                }
            }
            Ok(snapshot) => Err(format!(
                "display retained unexpected lock epoch {}",
                snapshot.lock_epoch
            )),
            Err(error) => Err(error),
        };
        if let Err(error) = display_result {
            return self.abort_wake(
                sleep_epoch,
                format!("{cause}; display-lock rollback failed: {error}"),
            );
        }
        self.state
            .write()
            .await
            .apply(Transition::Wake)
            .map_err(|error| error.to_string())?;
        self.sleep_transaction.finish_unlock(sleep_epoch)?;
        self.power.cancel_quiescing("sleep rollback").await;
        self.backlight.restore_desired();
        Err(cause)
    }
}

fn external_wake_locks_are_clear() -> bool {
    fs::read_to_string("/sys/power/wake_lock").is_ok_and(|value| wake_lock_text_is_clear(&value))
}

fn wake_lock_text_is_clear(value: &str) -> bool {
    value
        .split_whitespace()
        .all(|owner| owner == "remagic-managed")
}

fn wake_guard_is_already_armed(error: &str) -> bool {
    error == "power wake guard is already armed"
}

#[cfg(test)]
mod power_tests {
    use super::{wake_guard_is_already_armed, wake_lock_text_is_clear};

    #[test]
    fn blocked_retry_waits_for_the_charger_to_disappear() {
        assert!(!wake_lock_text_is_clear("remagic-managed udev.charger"));
        assert!(wake_lock_text_is_clear("remagic-managed"));
        assert!(wake_lock_text_is_clear(""));
    }

    #[test]
    fn stale_guard_recovery_is_exact() {
        assert!(wake_guard_is_already_armed(
            "power wake guard is already armed"
        ));
        assert!(!wake_guard_is_already_armed(
            "power wake guard is not armed"
        ));
    }
}
