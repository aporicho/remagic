use super::state::{ForegroundLease, HostState};
use crate::geometry::Rect;
use crate::panel::{PanelCommand, PanelLease, RefreshIntent};
use crate::protocol::{input_packet, INPUT_TOUCH_RELEASE};
use crate::protocol::{REFRESH_MODE_CONTENT, REFRESH_MODE_FAST, REFRESH_MODE_UFAST};
use std::io;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

mod lock;
mod registration;

impl HostState {
    pub fn set_foreground(
        &self,
        key: i32,
        generation: u64,
        epoch: u64,
        full_refresh: bool,
    ) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
        if self.lock.lock().unwrap().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "display is locked",
            ));
        }
        self.validate_foreground_fence(key, generation, epoch)?;
        let foreground = ForegroundLease {
            key,
            generation,
            epoch,
            ink_enabled: false,
        };
        self.enqueue_panel(PanelCommand::SetForeground {
            lease: foreground.panel_lease(),
            full_refresh,
        })?;
        self.commit_foreground_fence(key, generation, epoch);
        self.fence_input_contacts();
        *self.prepared_foreground.lock().unwrap() = None;
        *self.foreground.lock().unwrap() = Some(foreground);
        Ok(())
    }

    /// Fence the old visible lease before an application is asked to present
    /// its resume frame. Surface writes may continue, but no input or damage
    /// crosses the prepare/activate boundary.
    pub fn prepare_foreground(&self, key: i32, generation: u64, epoch: u64) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
        if self.lock.lock().unwrap().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "display is locked",
            ));
        }
        self.validate_foreground_fence(key, generation, epoch)?;
        let prepared = ForegroundLease {
            key,
            generation,
            epoch,
            ink_enabled: false,
        };
        self.commit_foreground_fence(key, generation, epoch);
        self.fence_input_contacts();
        *self.prepared_foreground.lock().unwrap() = Some(prepared);
        Ok(())
    }

    /// Atomically publish a prepared foreground image and its direct-ink
    /// policy. The panel worker configures ink before it commits the lease, so
    /// input can never observe a visible-but-unconfigured application.
    pub fn activate_foreground(
        &self,
        key: i32,
        generation: u64,
        epoch: u64,
        ink_enabled: bool,
        full_refresh: bool,
    ) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
        if self.lock.lock().unwrap().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "display is locked",
            ));
        }
        let prepared =
            self.prepared_foreground.lock().unwrap().ok_or_else(|| {
                io::Error::new(io::ErrorKind::PermissionDenied, "no prepared lease")
            })?;
        if prepared.key != key || prepared.generation != generation || prepared.epoch != epoch {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "activation does not match the prepared foreground lease",
            ));
        }
        let active = ForegroundLease {
            ink_enabled,
            ..prepared
        };
        self.enqueue_panel(PanelCommand::ActivateForeground {
            lease: active.panel_lease(),
            ink_enabled,
            full_refresh,
        })?;
        *self.foreground.lock().unwrap() = Some(active);
        *self.prepared_foreground.lock().unwrap() = None;
        Ok(())
    }

    fn validate_foreground_fence(&self, key: i32, generation: u64, epoch: u64) -> io::Result<()> {
        if generation == 0 || epoch == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "generation and foreground epoch must be non-zero",
            ));
        }
        if !self.surface_exists(key) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("surface {key} is not connected"),
            ));
        }
        let fences = self.fences.lock().unwrap();
        if let Some((previous_generation, previous_epoch)) = fences.get(&key).copied() {
            let stale = generation < previous_generation
                || (generation == previous_generation && epoch <= previous_epoch);
            if stale {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!(
                        "stale foreground fence for surface {key}: generation={generation} epoch={epoch}, previous={previous_generation}/{previous_epoch}"
                    ),
                ));
            }
        }
        Ok(())
    }

    fn commit_foreground_fence(&self, key: i32, generation: u64, epoch: u64) {
        self.fences.lock().unwrap().insert(key, (generation, epoch));
    }

    pub fn clear_foreground(&self) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
        if self.lock.lock().unwrap().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot clear foreground while display is locked",
            ));
        }
        *self.prepared_foreground.lock().unwrap() = None;
        let Some(lease) = self
            .foreground
            .lock()
            .unwrap()
            .map(ForegroundLease::panel_lease)
        else {
            return Ok(());
        };
        self.clear_foreground_locked(lease)
    }

    fn clear_foreground_locked(&self, lease: PanelLease) -> io::Result<()> {
        let matches = self
            .foreground
            .lock()
            .unwrap()
            .is_some_and(|current| current.panel_lease() == lease);
        if !matches {
            return Ok(());
        }
        self.enqueue_panel(PanelCommand::ClearForeground { lease })?;
        self.fence_input_contacts();
        let mut foreground = self.foreground.lock().unwrap();
        if foreground.is_some_and(|current| current.panel_lease() == lease) {
            *foreground = None;
        }
        Ok(())
    }

    pub fn configure_ink(
        &self,
        key: i32,
        generation: u64,
        epoch: u64,
        enabled: bool,
        region: Option<Rect>,
    ) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
        if self.lock.lock().unwrap().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "cannot configure direct ink while display is locked",
            ));
        }
        let mut foreground = self.foreground.lock().unwrap();
        let Some(lease) = foreground.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "no foreground lease",
            ));
        };
        if lease.key != key || lease.generation != generation || lease.epoch != epoch {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "ink request does not match foreground lease",
            ));
        }
        self.enqueue_panel(PanelCommand::ConfigureInk {
            lease: lease.panel_lease(),
            enabled,
            region,
        })?;
        lease.ink_enabled = enabled;
        let panel_lease = lease.panel_lease();
        drop(foreground);
        let deadline = Instant::now() + Duration::from_secs(2);
        while self.telemetry.committed_ink() != Some((panel_lease, enabled)) {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "panel did not acknowledge the direct-ink policy",
                ));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        Ok(())
    }

    pub fn request_full_refresh(&self) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
        if self.prepared_foreground.lock().unwrap().is_some() {
            return Ok(());
        }
        if let Some(lock) = *self.lock.lock().unwrap() {
            return self.enqueue_panel(PanelCommand::RefreshLock {
                lease: lock.foreground.panel_lease(),
                sleep_epoch: lock.sleep_epoch,
            });
        }
        let Some(lease) = self
            .foreground
            .lock()
            .unwrap()
            .map(ForegroundLease::panel_lease)
        else {
            return Ok(());
        };
        self.enqueue_panel(PanelCommand::FullRefresh { lease })
    }

    pub fn request_surface_full_refresh(&self, key: i32) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
        if self.prepared_foreground.lock().unwrap().is_some() {
            return Ok(());
        }
        if self.lock.lock().unwrap().is_some() {
            return Ok(());
        }
        let foreground = self
            .foreground
            .lock()
            .unwrap()
            .filter(|foreground| foreground.key == key)
            .map(ForegroundLease::panel_lease);
        let Some(lease) = foreground else {
            // A client commonly asks for cleanup while constructing its first
            // page. Until the manager grants its lease, that request must not
            // flash or repaint whichever application is actually visible.
            return Ok(());
        };
        self.enqueue_panel(PanelCommand::FullRefresh { lease })
    }

    pub fn damage(&self, key: i32, rect: Rect, intent: RefreshIntent) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
        if self.prepared_foreground.lock().unwrap().is_some() {
            return Ok(());
        }
        if self.lock.lock().unwrap().is_some() {
            return Ok(());
        }
        let foreground = self
            .foreground
            .lock()
            .unwrap()
            .filter(|foreground| foreground.key == key)
            .map(ForegroundLease::panel_lease);
        let Some(lease) = foreground else {
            return Ok(());
        };
        match self.enqueue_panel(PanelCommand::Damage {
            lease,
            rect,
            intent,
        }) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                // Damage is level-triggered canonical state, not a lifecycle
                // edge. Coalesce it for the panel worker instead of closing
                // the QTFB socket and killing an otherwise healthy app.
                self.telemetry.defer_damage(lease, rect, intent);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    pub fn mark_commit(&self, key: i32) -> io::Result<u64> {
        self.surfaces
            .lock()
            .unwrap()
            .get(&key)
            .map(|entry| entry.surface.mark_commit())
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "surface disconnected"))
    }

    pub(super) fn commit_damage(&self, key: i32, rect: Option<Rect>) -> io::Result<()> {
        self.mark_commit(key)?;
        let surfaces = self.surfaces.lock().unwrap();
        let surface = surfaces
            .get(&key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "surface disappeared"))?;
        let rect = rect.unwrap_or_else(|| surface.surface.full_rect());
        let intent = match surface.surface.refresh_mode() {
            REFRESH_MODE_UFAST => RefreshIntent::Ink,
            REFRESH_MODE_FAST => RefreshIntent::MonoQuality,
            REFRESH_MODE_CONTENT => RefreshIntent::Content,
            _ => RefreshIntent::Ui,
        };
        drop(surfaces);
        self.damage(key, rect, intent)
    }

    pub fn set_refresh_mode(&self, key: i32, mode: i32) -> io::Result<()> {
        let surfaces = self.surfaces.lock().unwrap();
        let surface = surfaces
            .get(&key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "surface disconnected"))?;
        surface.surface.set_refresh_mode(mode);
        Ok(())
    }

    fn fence_input_contacts(&self) {
        self.input_epoch.fetch_add(1, Ordering::AcqRel);
        if let Some((lease, last)) = self.active_pen.lock().unwrap().take() {
            let cancelled = crate::input::PenFrame {
                phase: crate::input::PenPhase::Cancel,
                pressure: 0,
                ..last
            };
            self.send_to_key(lease.key, &self.pen_packet(cancelled));
            let _ = self.enqueue_panel(PanelCommand::Pen {
                lease,
                frame: cancelled,
            });
            self.suppressed_pen.store(true, Ordering::Release);
        }
        let active = std::mem::take(&mut *self.active_touches.lock().unwrap());
        let foreground_key = {
            let foreground = self.foreground.lock().unwrap();
            foreground.map(|lease| lease.key)
        };
        if let Some(key) = foreground_key {
            for device_id in &active {
                self.send_to_key(key, &input_packet(INPUT_TOUCH_RELEASE, *device_id, 0, 0, 0));
            }
        }
        let mut suppressed = self.suppressed_touches.lock().unwrap();
        suppressed.extend(active);
    }
}
