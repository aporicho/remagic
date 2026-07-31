use super::super::super::{sleep, Daemon, Event, QueuedEvent};
use super::LOCK_AWAKE_TIMEOUT;
use std::fs;
use std::sync::atomic::Ordering;
use std::time::Duration;

impl Daemon {
    pub(super) fn schedule_locked_resuspend(&self, sleep: sleep::SleepSnapshot) {
        self.schedule_locked_resuspend_after(sleep, LOCK_AWAKE_TIMEOUT);
    }

    pub(super) fn schedule_locked_resuspend_after(
        &self,
        sleep: sleep::SleepSnapshot,
        delay: Duration,
    ) {
        let events = self.events.clone();
        let launch_interrupt_epoch = self.launch_interrupt_epoch.clone();
        let interaction_epoch = launch_interrupt_epoch.load(Ordering::Acquire);
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
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

    pub(super) fn schedule_failed_resuspend(
        &self,
        error: &str,
        sleep: sleep::SleepSnapshot,
        interaction_epoch: u64,
    ) {
        if suspend_error_requires_blocked_retry(error) {
            self.schedule_blocked_resuspend(sleep, interaction_epoch);
        } else {
            self.schedule_locked_resuspend(sleep);
        }
    }

    /// A charger wake lock is expected while USB power is attached. Keep the
    /// lock page committed and retry after every external blocker has
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
                tokio::time::sleep(Duration::from_secs(2)).await;
                if launch_interrupt_epoch.load(Ordering::Acquire) != interaction_epoch {
                    return;
                }
                if suspend_retry_blockers_are_clear() {
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
}

fn suspend_retry_blockers_are_clear() -> bool {
    fs::read_to_string("/sys/power/wake_lock").is_ok_and(|value| wake_lock_text_is_clear(&value))
        && active_wakeup_sources_are_clear()
}

fn wake_lock_text_is_clear(value: &str) -> bool {
    value
        .split_whitespace()
        .all(|owner| owner == "remagic-managed")
}

fn suspend_error_requires_blocked_retry(error: &str) -> bool {
    error.starts_with("kernel suspend is blocked by active wake locks:")
        || error.contains("active wakeup sources:")
}

fn active_wakeup_sources_are_clear() -> bool {
    let entries = match fs::read_dir("/sys/class/wakeup") {
        Ok(entries) => entries,
        Err(_) => return true,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = fs::read_to_string(path.join("name"))
            .ok()
            .map(|value| value.trim().to_owned())
            .unwrap_or_default();
        if name == "remagic-managed" {
            continue;
        }
        let active_time_ms = fs::read_to_string(path.join("active_time_ms"))
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(0);
        if active_time_ms > 0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{suspend_error_requires_blocked_retry, wake_lock_text_is_clear};

    #[test]
    fn blocked_retry_waits_for_the_charger_to_disappear() {
        assert!(!wake_lock_text_is_clear("remagic-managed udev.charger"));
        assert!(wake_lock_text_is_clear("remagic-managed"));
        assert!(wake_lock_text_is_clear(""));
    }

    #[test]
    fn external_or_active_wakeup_errors_wait_for_blockers_to_clear() {
        assert!(suspend_error_requires_blocked_retry(
            "kernel suspend is blocked by active wake locks: udev.charger"
        ));
        assert!(suspend_error_requires_blocked_retry(
            "kernel autosleep did not complete within 90000 ms; active wakeup sources: 1-0048(active_ms=42)"
        ));
        assert!(!suspend_error_requires_blocked_retry(
            "systemctl start --wait systemd-suspend.service failed: failed"
        ));
        assert!(!suspend_error_requires_blocked_retry(
            "system suspend returned without advancing kernel suspend counter"
        ));
    }
}
