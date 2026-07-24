use super::PanelRuntime;
use crate::geometry::{Geometry, Rect};
use crate::panel::render::{copy_surface_rect, sampled_signature};
use crate::panel::{PanelBackend, PanelLease, RefreshIntent, SubmissionReason, SubmissionRecord};
use crate::protocol::{
    REFRESH_MODE_ANIMATE, REFRESH_MODE_CONTENT, REFRESH_MODE_FAST, REFRESH_MODE_UFAST,
};
use crate::surface::SharedSurface;
use std::io;
use std::sync::atomic::Ordering;

impl<B: PanelBackend> PanelRuntime<B> {
    pub(in crate::panel) fn present_surface(
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
    pub(in crate::panel) fn sync_surface_buffer(
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
            _ => RefreshIntent::MonoQuality,
        }
    }

    pub(in crate::panel) fn submit(
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
                self.record_submission(
                    lease,
                    surface_sequence,
                    intent,
                    reason,
                    signature,
                    Some(marker),
                    true,
                );
                Ok(marker)
            }
            Err(error) => {
                self.telemetry.failure_count.fetch_add(1, Ordering::AcqRel);
                self.record_submission(
                    lease,
                    surface_sequence,
                    intent,
                    reason,
                    signature,
                    None,
                    false,
                );
                Err(error)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_submission(
        &self,
        lease: PanelLease,
        surface_sequence: u64,
        intent: RefreshIntent,
        reason: SubmissionReason,
        visible_signature: u64,
        marker: Option<u64>,
        success: bool,
    ) {
        self.telemetry.record_submission(SubmissionRecord {
            sequence: 0,
            surface_sequence,
            key: lease.key,
            generation: lease.generation,
            foreground_epoch: lease.foreground_epoch,
            intent,
            reason,
            visible_signature,
            marker,
            success,
        });
    }

    pub(in crate::panel) fn geometry_for_logical(&self, width: i32, height: i32) -> Geometry {
        Geometry::new(width, height, self.backend.width(), self.backend.height()).unwrap()
    }
}
