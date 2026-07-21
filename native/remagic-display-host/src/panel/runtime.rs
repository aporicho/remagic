use super::render::{copy_surface_rect, sampled_signature, LivePenPoint};
use super::{
    PanelBackend, PanelCommand, PanelLease, PanelTelemetry, RefreshIntent, SubmissionReason,
    SubmissionRecord,
};
use crate::geometry::{Geometry, Rect};
use crate::protocol::{
    REFRESH_MODE_ANIMATE, REFRESH_MODE_CONTENT, REFRESH_MODE_FAST, REFRESH_MODE_UFAST,
};
use crate::surface::SharedSurface;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) const LIVE_SWAP_INTERVAL: Duration = Duration::from_millis(8);
pub(super) const CANONICAL_SETTLE_DELAY: Duration = Duration::from_millis(280);
pub(super) const CANONICAL_SETTLE_RETRY: Duration = Duration::from_millis(40);
pub(super) const CANONICAL_SETTLE_LIMIT: Duration = Duration::from_millis(800);

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct InkLease {
    pub(super) key: i32,
    pub(super) generation: u64,
    pub(super) epoch: u64,
    pub(super) enabled: bool,
    pub(super) region: Option<Rect>,
}

pub struct PanelRuntime<B: PanelBackend> {
    pub(super) backend: B,
    receiver: Receiver<PanelCommand>,
    pub(super) surfaces: HashMap<i32, Arc<SharedSurface>>,
    pub(super) foreground: Option<PanelLease>,
    pub(super) ink: InkLease,
    pub(super) active_pen: bool,
    pub(super) last_pen: Option<LivePenPoint>,
    pub(super) live_dirty: Rect,
    pub(super) canonical_dirty: Rect,
    pub(super) last_live_submit: Instant,
    pub(super) settle_deadline: Option<Instant>,
    pub(super) settle_started: Option<Instant>,
    pub(super) ink_begin_sequence: u64,
    telemetry: Arc<PanelTelemetry>,
}

impl<B: PanelBackend> PanelRuntime<B> {
    pub fn new(backend: B, receiver: Receiver<PanelCommand>) -> Self {
        Self::with_telemetry(backend, receiver, Arc::new(PanelTelemetry::default()))
    }

    pub fn with_telemetry(
        backend: B,
        receiver: Receiver<PanelCommand>,
        telemetry: Arc<PanelTelemetry>,
    ) -> Self {
        Self {
            backend,
            receiver,
            surfaces: HashMap::new(),
            foreground: None,
            ink: InkLease::default(),
            active_pen: false,
            last_pen: None,
            live_dirty: Rect::default(),
            canonical_dirty: Rect::default(),
            last_live_submit: Instant::now() - LIVE_SWAP_INTERVAL,
            settle_deadline: None,
            settle_started: None,
            ink_begin_sequence: 0,
            telemetry,
        }
    }

    pub fn run(mut self) -> io::Result<()> {
        loop {
            let timeout = self.next_timeout();
            match self.receiver.recv_timeout(timeout) {
                Ok(command) => {
                    self.telemetry.command_dequeued();
                    if matches!(command, PanelCommand::Shutdown) {
                        break;
                    }
                    self.handle(command)?;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.flush_deadlines()?;
            self.backend.process_events();
        }
        Ok(())
    }

    fn next_timeout(&self) -> Duration {
        let now = Instant::now();
        let mut timeout = Duration::from_millis(50);
        if !self.live_dirty.is_empty() {
            timeout = timeout
                .min((self.last_live_submit + LIVE_SWAP_INTERVAL).saturating_duration_since(now));
        }
        if let Some(deadline) = self.settle_deadline {
            timeout = timeout.min(deadline.saturating_duration_since(now));
        }
        timeout
    }

    fn handle(&mut self, command: PanelCommand) -> io::Result<()> {
        match command {
            PanelCommand::RegisterSurface(surface) => {
                self.surfaces.insert(surface.key, surface);
            }
            PanelCommand::DropSurface { key } => self.drop_surface(key),
            PanelCommand::Damage {
                lease,
                rect,
                intent,
            } => self.handle_damage(lease, rect, intent)?,
            PanelCommand::SetForeground {
                lease,
                full_refresh,
            } => self.set_foreground(lease, full_refresh)?,
            PanelCommand::ClearForeground { lease } => self.clear_foreground(lease),
            PanelCommand::ConfigureInk {
                lease,
                enabled,
                region,
            } => self.configure_ink(lease, enabled, region)?,
            PanelCommand::Pen { lease, frame } => self.handle_pen(lease, frame)?,
            PanelCommand::FullRefresh { lease } => self.full_refresh(lease)?,
            PanelCommand::Shutdown => unreachable!(),
        }
        Ok(())
    }

    fn drop_surface(&mut self, key: i32) {
        self.surfaces.remove(&key);
        if self
            .foreground
            .is_some_and(|foreground| foreground.key == key)
        {
            self.clear_foreground_unchecked();
        }
    }

    fn handle_damage(
        &mut self,
        lease: PanelLease,
        rect: Rect,
        intent: RefreshIntent,
    ) -> io::Result<()> {
        if self.foreground != Some(lease) {
            return Ok(());
        }
        if (self.active_pen || self.settle_deadline.is_some())
            && self.ink.enabled
            && self.ink.key == lease.key
        {
            self.canonical_dirty = self.canonical_dirty.union(rect);
            Ok(())
        } else {
            self.present_surface(lease, rect, intent, SubmissionReason::SurfaceDamage)
        }
    }

    fn set_foreground(&mut self, lease: PanelLease, full_refresh: bool) -> io::Result<()> {
        let Some(surface) = self.surfaces.get(&lease.key).cloned() else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("foreground surface {} is not connected", lease.key),
            ));
        };
        self.abort_ink();
        self.foreground = Some(lease);
        let intent = if full_refresh {
            RefreshIntent::Full
        } else {
            RefreshIntent::Content
        };
        self.present_surface(
            lease,
            surface.full_rect(),
            intent,
            SubmissionReason::ForegroundSwitch,
        )?;
        self.telemetry.commit_foreground(lease);
        Ok(())
    }

    fn configure_ink(
        &mut self,
        lease: PanelLease,
        enabled: bool,
        region: Option<Rect>,
    ) -> io::Result<()> {
        let valid = self.foreground == Some(lease);
        if enabled && !valid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "direct ink lease does not match the foreground epoch",
            ));
        }
        self.abort_ink();
        self.ink = InkLease {
            key: lease.key,
            generation: lease.generation,
            epoch: lease.foreground_epoch,
            enabled,
            region,
        };
        Ok(())
    }

    fn full_refresh(&mut self, lease: PanelLease) -> io::Result<()> {
        if self.foreground != Some(lease) {
            return Ok(());
        }
        let Some(surface) = self.surfaces.get(&lease.key) else {
            return Ok(());
        };
        self.present_surface(
            lease,
            surface.full_rect(),
            RefreshIntent::Full,
            SubmissionReason::FullRefresh,
        )
    }

    pub(super) fn present_surface(
        &mut self,
        lease: PanelLease,
        logical_rect: Rect,
        intent: RefreshIntent,
        reason: SubmissionReason,
    ) -> io::Result<()> {
        if self.foreground != Some(lease) {
            return Ok(());
        }
        let Some(surface) = self.surfaces.get(&lease.key).cloned() else {
            return Ok(());
        };
        let logical_rect = logical_rect.clip(surface.width, surface.height);
        if logical_rect.is_empty() {
            return Ok(());
        }
        let geometry = self.geometry_for_logical(surface.width, surface.height);
        let physical_rect = geometry.logical_to_physical_rect(logical_rect);
        let destination_stride = self.backend.stride();
        copy_surface_rect(
            &surface,
            self.backend.pixels_mut(),
            destination_stride,
            geometry,
            physical_rect,
        );
        let effective = self.effective_intent(&surface, intent);
        let surface_sequence = surface.commit_sequence();
        self.submit(physical_rect, effective, lease, surface_sequence, reason)?;
        self.telemetry.mark_presented(lease.key, surface_sequence);
        Ok(())
    }

    /// Copy the authoritative application surface into the host framebuffer
    /// without touching the panel. Live ink is already visible on glass; the
    /// settle step only needs to make future application damage start from the
    /// canonical pixels. Coupling this copy to a panel submission caused a
    /// visible flash after every pen-up.
    pub(super) fn sync_surface_buffer(
        &mut self,
        lease: PanelLease,
        logical_rect: Rect,
    ) -> io::Result<()> {
        if self.foreground != Some(lease) {
            return Ok(());
        }
        let Some(surface) = self.surfaces.get(&lease.key).cloned() else {
            return Ok(());
        };
        let logical_rect = logical_rect.clip(surface.width, surface.height);
        if logical_rect.is_empty() {
            return Ok(());
        }
        let geometry = self.geometry_for_logical(surface.width, surface.height);
        let physical_rect = geometry.logical_to_physical_rect(logical_rect);
        let destination_stride = self.backend.stride();
        copy_surface_rect(
            &surface,
            self.backend.pixels_mut(),
            destination_stride,
            geometry,
            physical_rect,
        );
        Ok(())
    }

    fn effective_intent(&self, surface: &SharedSurface, intent: RefreshIntent) -> RefreshIntent {
        if intent != RefreshIntent::Ui {
            return intent;
        }
        match surface.refresh_mode() {
            REFRESH_MODE_UFAST => RefreshIntent::Ink,
            REFRESH_MODE_FAST => RefreshIntent::MonoQuality,
            REFRESH_MODE_ANIMATE => RefreshIntent::Ui,
            REFRESH_MODE_CONTENT => RefreshIntent::Content,
            _ => RefreshIntent::Ui,
        }
    }

    pub(super) fn submit(
        &mut self,
        rect: Rect,
        intent: RefreshIntent,
        lease: PanelLease,
        surface_sequence: u64,
        reason: SubmissionReason,
    ) -> io::Result<u64> {
        let stride = self.backend.stride();
        let signature = sampled_signature(self.backend.pixels_mut(), stride, rect);
        match self.backend.submit(rect, intent) {
            Ok(marker) => {
                if intent == RefreshIntent::Full {
                    self.telemetry.mark_full_refresh();
                }
                self.telemetry
                    .submission_count
                    .fetch_add(1, Ordering::AcqRel);
                self.telemetry.last_marker.store(marker, Ordering::Release);
                self.telemetry
                    .visible_signature
                    .store(signature, Ordering::Release);
                self.telemetry.record_submission(SubmissionRecord {
                    sequence: 0,
                    surface_sequence,
                    key: lease.key,
                    generation: lease.generation,
                    foreground_epoch: lease.foreground_epoch,
                    intent,
                    reason,
                    visible_signature: signature,
                    marker: Some(marker),
                    success: true,
                });
                Ok(marker)
            }
            Err(error) => {
                self.telemetry.failure_count.fetch_add(1, Ordering::AcqRel);
                self.telemetry.record_submission(SubmissionRecord {
                    sequence: 0,
                    surface_sequence,
                    key: lease.key,
                    generation: lease.generation,
                    foreground_epoch: lease.foreground_epoch,
                    intent,
                    reason,
                    visible_signature: signature,
                    marker: None,
                    success: false,
                });
                Err(error)
            }
        }
    }

    pub(super) fn geometry_for_logical(&self, width: i32, height: i32) -> Geometry {
        Geometry::new(width, height, self.backend.width(), self.backend.height()).unwrap()
    }

    pub(super) fn abort_ink(&mut self) {
        self.active_pen = false;
        self.last_pen = None;
        self.live_dirty = Rect::default();
        self.canonical_dirty = Rect::default();
        self.settle_deadline = None;
        self.settle_started = None;
        self.ink_begin_sequence = 0;
    }

    fn clear_foreground(&mut self, lease: PanelLease) {
        if self.foreground != Some(lease) {
            return;
        }
        self.clear_foreground_unchecked();
    }

    fn clear_foreground_unchecked(&mut self) {
        if let Some(lease) = self.foreground {
            self.telemetry.clear_committed_foreground(lease);
        }
        self.abort_ink();
        self.foreground = None;
        self.ink = InkLease::default();
    }
}

#[cfg(test)]
mod tests;
