use super::queue::InputQueue;
use super::state::{ClientSink, ForegroundLease, HostState, SurfaceEntry};
use crate::geometry::Rect;
use crate::panel::{PanelCommand, PanelLease, RefreshIntent};
use crate::protocol::{PixelFormat, REFRESH_MODE_CONTENT, REFRESH_MODE_FAST, REFRESH_MODE_UFAST};
use crate::surface::SharedSurface;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::Ordering;
use std::sync::Arc;

impl HostState {
    pub(super) fn register(
        &self,
        key: i32,
        width: i32,
        height: i32,
        format: PixelFormat,
    ) -> io::Result<Arc<SharedSurface>> {
        let mut surfaces = self.surfaces.lock().unwrap();
        if let Some(entry) = surfaces.get_mut(&key) {
            Self::validate_surface(entry, width, height, format)?;
            // A QTFB key is an application-owned writable framebuffer, not a
            // broadcast channel. Sharing it between two live clients would
            // allow a stale process to overwrite the current application's
            // pixels and would duplicate every input frame. The runner stops
            // the old cgroup before replacement, so a legitimate reconnect
            // happens only after `unregister` removes the previous surface.
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("QTFB surface {key} already has a live owner"),
            ));
        }
        let surface = self.create_surface(key, width, height, format)?;
        surfaces.insert(
            key,
            SurfaceEntry {
                surface: Arc::clone(&surface),
                clients: Vec::new(),
            },
        );
        self.enqueue_panel(PanelCommand::RegisterSurface(Arc::clone(&surface)))?;
        Ok(surface)
    }

    pub(super) fn activate_client(
        &self,
        key: i32,
        client: RawFd,
        input_queue: Arc<InputQueue>,
    ) -> io::Result<()> {
        let mut surfaces = self.surfaces.lock().unwrap();
        let entry = surfaces
            .get_mut(&key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "QTFB surface disappeared"))?;
        if !entry.clients.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("QTFB surface {key} already has a live owner"),
            ));
        }
        entry.clients.push(ClientSink {
            id: client,
            queue: input_queue,
        });
        Ok(())
    }

    pub(super) fn abort_registration(&self, key: i32) {
        let mut surfaces = self.surfaces.lock().unwrap();
        let can_remove = surfaces
            .get(&key)
            .is_some_and(|entry| entry.clients.is_empty());
        if can_remove {
            surfaces.remove(&key);
            let _ = self.enqueue_panel(PanelCommand::DropSurface { key });
        }
    }

    fn validate_surface(
        entry: &SurfaceEntry,
        width: i32,
        height: i32,
        format: PixelFormat,
    ) -> io::Result<()> {
        if entry.surface.width == width
            && entry.surface.height == height
            && entry.surface.format == format
        {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "QTFB key was reused with incompatible geometry or format",
        ))
    }

    fn create_surface(
        &self,
        key: i32,
        width: i32,
        height: i32,
        format: PixelFormat,
    ) -> io::Result<Arc<SharedSurface>> {
        loop {
            let candidate =
                (self.next_shm_key.fetch_add(1, Ordering::Relaxed) & 0x7fff_ffff).max(1) as i32;
            match SharedSurface::create(key, width, height, format, candidate) {
                Ok(surface) => return Ok(Arc::new(surface)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) fn unregister(&self, client: RawFd, key: Option<i32>) {
        let Some(key) = key else { return };
        let mut surfaces = self.surfaces.lock().unwrap();
        let remove = if let Some(entry) = surfaces.get_mut(&key) {
            entry.clients.retain(|candidate| candidate.id != client);
            entry.clients.is_empty()
        } else {
            false
        };
        if !remove {
            return;
        }
        let lease_to_clear = self
            .foreground
            .lock()
            .unwrap()
            .filter(|foreground| foreground.key == key)
            .map(ForegroundLease::panel_lease);
        surfaces.remove(&key);
        let _ = self.enqueue_panel(PanelCommand::DropSurface { key });
        drop(surfaces);
        if let Some(lease) = lease_to_clear {
            let _ = self.clear_foreground_if(lease);
        }
    }

    pub fn surface_exists(&self, key: i32) -> bool {
        self.surfaces.lock().unwrap().contains_key(&key)
    }

    pub fn set_foreground(
        &self,
        key: i32,
        generation: u64,
        epoch: u64,
        full_refresh: bool,
    ) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
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
        *self.foreground.lock().unwrap() = Some(foreground);
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
        let mut fences = self.fences.lock().unwrap();
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
        fences.insert(key, (generation, epoch));
        Ok(())
    }

    pub fn clear_foreground(&self) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
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

    fn clear_foreground_if(&self, lease: PanelLease) -> io::Result<()> {
        let _operation = self.foreground_ops.lock().unwrap();
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
        Ok(())
    }

    pub fn request_full_refresh(&self) -> io::Result<()> {
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
        let foreground = self
            .foreground
            .lock()
            .unwrap()
            .filter(|foreground| foreground.key == key)
            .map(ForegroundLease::panel_lease);
        let Some(lease) = foreground else {
            return Ok(());
        };
        self.enqueue_panel(PanelCommand::Damage {
            lease,
            rect,
            intent,
        })
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
}
