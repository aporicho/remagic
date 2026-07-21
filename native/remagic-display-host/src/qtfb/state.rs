use super::queue::InputQueue;
use crate::panel::{PanelCommand, PanelLease, PanelTelemetry};
use crate::protocol::DisplaySnapshot;
use crate::surface::SharedSurface;
use std::collections::HashMap;
use std::io;
use std::os::fd::RawFd;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex};

pub(super) struct SurfaceEntry {
    pub(super) surface: Arc<SharedSurface>,
    pub(super) clients: Vec<ClientSink>,
}

pub(super) struct ClientSink {
    pub(super) id: RawFd,
    pub(super) queue: Arc<InputQueue>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct ForegroundLease {
    pub(super) key: i32,
    pub(super) generation: u64,
    pub(super) epoch: u64,
    pub(super) ink_enabled: bool,
}

impl ForegroundLease {
    pub(super) fn panel_lease(self) -> PanelLease {
        PanelLease {
            key: self.key,
            generation: self.generation,
            foreground_epoch: self.epoch,
        }
    }
}

pub struct HostState {
    pub(super) surfaces: Mutex<HashMap<i32, SurfaceEntry>>,
    pub(super) foreground: Mutex<Option<ForegroundLease>>,
    pub(super) foreground_ops: Mutex<()>,
    pub(super) fences: Mutex<HashMap<i32, (u64, u64)>>,
    panel: SyncSender<PanelCommand>,
    pub(super) next_shm_key: AtomicU32,
    physical_width: i32,
    physical_height: i32,
    physical_stride: usize,
    shutdown: AtomicBool,
    pub(super) input_backpressure: AtomicU64,
    pub(super) telemetry: Arc<PanelTelemetry>,
    pub(super) injected_sequence: AtomicU64,
}

impl HostState {
    pub fn new(
        panel: SyncSender<PanelCommand>,
        physical_width: i32,
        physical_height: i32,
        physical_stride: usize,
    ) -> Arc<Self> {
        Self::new_with_telemetry(
            panel,
            physical_width,
            physical_height,
            physical_stride,
            Arc::new(PanelTelemetry::default()),
        )
    }

    pub fn new_with_telemetry(
        panel: SyncSender<PanelCommand>,
        physical_width: i32,
        physical_height: i32,
        physical_stride: usize,
        telemetry: Arc<PanelTelemetry>,
    ) -> Arc<Self> {
        Arc::new(Self {
            surfaces: Mutex::new(HashMap::new()),
            foreground: Mutex::new(None),
            foreground_ops: Mutex::new(()),
            fences: Mutex::new(HashMap::new()),
            panel,
            next_shm_key: AtomicU32::new(1),
            physical_width,
            physical_height,
            physical_stride,
            shutdown: AtomicBool::new(false),
            input_backpressure: AtomicU64::new(0),
            telemetry,
            injected_sequence: AtomicU64::new(1),
        })
    }

    pub fn snapshot(&self) -> DisplaySnapshot {
        let surfaces = self.surfaces.lock().unwrap();
        let requested_foreground = *self.foreground.lock().unwrap();
        let foreground = self.telemetry.committed_foreground();
        let (panel_submission_count, panel_last_marker, panel_failure_count, visible_signature) =
            self.telemetry.snapshot();
        let last_presented = self.telemetry.last_presented();
        DisplaySnapshot {
            physical_width: self.physical_width,
            physical_height: self.physical_height,
            stride: self.physical_stride,
            surfaces: surfaces.keys().copied().collect(),
            surface_sequences: surfaces
                .iter()
                .map(|(key, entry)| (*key, entry.surface.commit_sequence()))
                .collect(),
            surface_signatures: surfaces
                .iter()
                .map(|(key, entry)| (*key, surface_signature(&entry.surface)))
                .collect(),
            foreground_key: foreground.map(|value| value.key),
            generation: foreground.map_or(0, |value| value.generation),
            foreground_epoch: foreground.map_or(0, |value| value.foreground_epoch),
            ink_enabled: foreground.is_some_and(|committed| {
                requested_foreground.is_some_and(|requested| {
                    requested.panel_lease() == committed && requested.ink_enabled
                })
            }),
            queue_depth: self.telemetry.queue_depth(),
            input_backpressure_events: self.input_backpressure.load(Ordering::Relaxed),
            panel_submission_count,
            panel_last_marker,
            panel_failure_count,
            visible_signature,
            full_refresh_count: self.telemetry.full_refresh_count(),
            last_presented_key: last_presented.map(|value| value.0),
            last_presented_sequence: last_presented.map_or(0, |value| value.1),
            recent_submissions: self.telemetry.recent_submissions(),
        }
    }

    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.enqueue_panel(PanelCommand::Shutdown);
    }

    pub fn is_shutdown(&self) -> bool {
        self.shutdown.load(Ordering::Acquire)
    }

    pub(super) fn enqueue_panel(&self, command: PanelCommand) -> io::Result<()> {
        self.telemetry.command_enqueued();
        if self.panel.send(command).is_err() {
            self.telemetry.command_dequeued();
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "panel thread stopped",
            ));
        }
        Ok(())
    }
}

fn surface_signature(surface: &SharedSurface) -> u64 {
    let bytes = surface.bytes();
    let row_bytes = surface.width.max(0) as usize * surface.format.bytes_per_pixel();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for y in (0..surface.height.max(0) as usize).step_by(16) {
        let start = y.saturating_mul(surface.stride);
        let end = start.saturating_add(row_bytes).min(bytes.len());
        for byte in bytes.get(start..end).unwrap_or_default().iter().step_by(32) {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
    }
    hash ^ ((surface.width as u64) << 32) ^ surface.height as u64
}
