use super::HostState;
use crate::geometry::Rect;
use crate::panel::PanelCommand;
use crate::qtfb::state::{ForegroundLease, LockLease};
use std::io;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

impl HostState {
    pub fn show_lock(
        &self,
        key: i32,
        generation: u64,
        epoch: u64,
        sleep_epoch: u64,
        unlock_region: Rect,
    ) -> io::Result<()> {
        if sleep_epoch == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "sleep epoch must be non-zero",
            ));
        }
        let _operation = self.foreground_ops.lock().unwrap();
        if self.prepared_foreground.lock().unwrap().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "a foreground lease is being prepared",
            ));
        }
        if let Some(current) = *self.lock.lock().unwrap() {
            if current.sleep_epoch == sleep_epoch
                && current.foreground.key == key
                && current.foreground.generation == generation
                && current.foreground.epoch == epoch
                && current.unlock_region == unlock_region
            {
                return Ok(());
            }
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "display is already locked for epoch {}",
                    current.sleep_epoch
                ),
            ));
        }
        let previous_sleep_epoch = self.last_sleep_epoch.load(Ordering::Acquire);
        if sleep_epoch <= previous_sleep_epoch {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "stale sleep epoch {sleep_epoch}; highest observed epoch is {previous_sleep_epoch}"
                ),
            ));
        }
        self.validate_foreground_fence(key, generation, epoch)?;
        self.validate_unlock_region(key, unlock_region)?;
        let foreground = ForegroundLease {
            key,
            generation,
            epoch,
            ink_enabled: false,
        };
        self.enqueue_panel(PanelCommand::ShowLock {
            lease: foreground.panel_lease(),
            sleep_epoch,
        })?;
        self.commit_foreground_fence(key, generation, epoch);
        self.last_sleep_epoch.store(sleep_epoch, Ordering::Release);
        self.fence_input_contacts();
        *self.foreground.lock().unwrap() = Some(foreground);
        *self.lock.lock().unwrap() = Some(LockLease {
            sleep_epoch,
            foreground,
            unlock_region,
        });
        self.lock_touches.lock().unwrap().clear();
        Ok(())
    }

    fn validate_unlock_region(&self, key: i32, unlock_region: Rect) -> io::Result<()> {
        let surfaces = self.surfaces.lock().unwrap();
        let surface = &surfaces
            .get(&key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "lock surface disappeared"))?
            .surface;
        if unlock_region.is_empty()
            || unlock_region.clip(surface.width, surface.height) != unlock_region
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unlock region is outside the lock surface",
            ));
        }
        Ok(())
    }

    pub fn refresh_lock(&self, sleep_epoch: u64) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
        let lock = self.require_lock(sleep_epoch)?;
        self.enqueue_panel(PanelCommand::RefreshLock {
            lease: lock.foreground.panel_lease(),
            sleep_epoch,
        })
    }

    pub fn cancel_lock(
        &self,
        sleep_epoch: u64,
        replacement_surface_sequence: u64,
    ) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
        let Some(lock) = self.cancellable_lock(sleep_epoch)? else {
            return Ok(());
        };
        if replacement_surface_sequence == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unlock replacement sequence must be non-zero",
            ));
        }
        self.validate_replacement_sequence(lock, replacement_surface_sequence)?;
        self.enqueue_and_wait_for_cancellation(lock, replacement_surface_sequence)?;
        self.fence_input_contacts();
        *self.lock.lock().unwrap() = None;
        self.last_cancelled_sleep_epoch
            .store(sleep_epoch, Ordering::Release);
        Ok(())
    }

    fn cancellable_lock(&self, sleep_epoch: u64) -> io::Result<Option<LockLease>> {
        let Some(lock) = *self.lock.lock().unwrap() else {
            if self.last_cancelled_sleep_epoch.load(Ordering::Acquire) == sleep_epoch {
                return Ok(None);
            }
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("stale or absent display lock epoch {sleep_epoch}"),
            ));
        };
        if lock.sleep_epoch != sleep_epoch {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("stale display lock epoch {sleep_epoch}"),
            ));
        }
        Ok(Some(lock))
    }

    fn validate_replacement_sequence(
        &self,
        lock: LockLease,
        replacement_surface_sequence: u64,
    ) -> io::Result<()> {
        let observed_sequence = self
            .surfaces
            .lock()
            .unwrap()
            .get(&lock.foreground.key)
            .map(|entry| entry.surface.commit_sequence())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unlock surface disappeared"))?;
        if observed_sequence < replacement_surface_sequence {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "unlock surface sequence {observed_sequence} has not reached {replacement_surface_sequence}"
                ),
            ));
        }
        let committed = self.telemetry.committed_lock_epoch();
        if committed != 0 && committed != lock.sleep_epoch {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("panel retained a different lock epoch {committed}"),
            ));
        }
        Ok(())
    }

    fn enqueue_and_wait_for_cancellation(
        &self,
        lock: LockLease,
        replacement_surface_sequence: u64,
    ) -> io::Result<()> {
        if self.telemetry.cancelled_lock_epoch() == lock.sleep_epoch {
            return Ok(());
        }
        // Queue ordering makes this a barrier after a pending ShowLock, even
        // while committed_lock_epoch is still zero.
        self.enqueue_panel(PanelCommand::CancelLock {
            lease: lock.foreground.panel_lease(),
            sleep_epoch: lock.sleep_epoch,
            replacement_surface_sequence,
        })?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.telemetry.cancelled_lock_epoch() != lock.sleep_epoch {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "panel did not acknowledge lock cancellation",
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    fn require_lock(&self, sleep_epoch: u64) -> io::Result<LockLease> {
        self.lock
            .lock()
            .unwrap()
            .filter(|lock| lock.sleep_epoch == sleep_epoch)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("stale or absent display lock epoch {sleep_epoch}"),
                )
            })
    }
}
