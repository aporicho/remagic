use super::HostState;
use crate::panel::PanelCommand;
use crate::protocol::PixelFormat;
use crate::qtfb::queue::InputQueue;
use crate::qtfb::state::{ClientSink, ForegroundLease, SurfaceEntry};
use crate::surface::SharedSurface;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::Ordering;
use std::sync::Arc;

impl HostState {
    pub(in crate::qtfb) fn register(
        &self,
        key: i32,
        width: i32,
        height: i32,
        format: PixelFormat,
    ) -> io::Result<Arc<SharedSurface>> {
        let mut surfaces = self.surfaces.lock().unwrap();
        if let Some(entry) = surfaces.get_mut(&key) {
            Self::validate_surface(entry, width, height, format)?;
            // A key is an application-owned framebuffer, not a broadcast
            // channel. A reconnect is valid only after unregister removes it.
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
        if let Err(error) = self.enqueue_panel(PanelCommand::RegisterSurface(Arc::clone(&surface)))
        {
            surfaces.remove(&key);
            return Err(error);
        }
        Ok(surface)
    }

    pub(in crate::qtfb) fn activate_client(
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

    pub(in crate::qtfb) fn abort_registration(&self, key: i32) {
        let mut surfaces = self.surfaces.lock().unwrap();
        let can_remove = surfaces
            .get(&key)
            .is_some_and(|entry| entry.clients.is_empty());
        if can_remove {
            surfaces.remove(&key);
            if let Err(error) = self.enqueue_panel(PanelCommand::DropSurface { key }) {
                self.fail_closed("could not roll back panel surface registration", &error);
            }
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

    pub(in crate::qtfb) fn unregister(&self, client: RawFd, key: Option<i32>) {
        let Some(key) = key else { return };
        let _operation = self.foreground_ops.lock().unwrap();
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
            .filter(|foreground| {
                foreground.key == key
                    && !self
                        .lock
                        .lock()
                        .unwrap()
                        .is_some_and(|lock| lock.foreground.key == key)
            })
            .map(ForegroundLease::panel_lease);
        surfaces.remove(&key);
        let drop_error = self.enqueue_panel(PanelCommand::DropSurface { key }).err();
        {
            let mut prepared = self.prepared_foreground.lock().unwrap();
            if prepared.is_some_and(|lease| lease.key == key) {
                *prepared = None;
            }
        }
        drop(surfaces);
        if let Some(lease) = lease_to_clear {
            self.fence_input_contacts();
            if let Err(error) = self.clear_foreground_locked(lease) {
                self.fail_closed("could not clear disconnected foreground surface", &error);
            }
        }
        if let Some(error) = drop_error {
            self.fail_closed("could not remove disconnected panel surface", &error);
        }
    }

    pub fn surface_exists(&self, key: i32) -> bool {
        self.surfaces.lock().unwrap().contains_key(&key)
    }
}
