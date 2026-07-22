use super::render::{
    draw_line, live_brush_radius, live_segment_radius, point_inside, LivePenPoint,
};
use super::runtime::{
    PanelRuntime, CANONICAL_SETTLE_DELAY, CANONICAL_SETTLE_LIMIT, CANONICAL_SETTLE_RETRY,
    LIVE_SWAP_INTERVAL,
};
use super::{PanelBackend, PanelLease, RefreshIntent, SubmissionReason};
use crate::geometry::Rect;
use crate::input::{PenFrame, PenPhase, PenTool};
use std::io;
use std::time::Instant;

impl<B: PanelBackend> PanelRuntime<B> {
    pub(super) fn handle_pen(&mut self, lease: PanelLease, frame: PenFrame) -> io::Result<()> {
        let valid_lease = self.ink.enabled
            && self.foreground == Some(lease)
            && self.ink.key == lease.key
            && self.ink.generation == lease.generation
            && self.ink.epoch == lease.foreground_epoch
            && self.foreground.is_some_and(|foreground| {
                foreground.key == self.ink.key
                    && foreground.generation == self.ink.generation
                    && foreground.foreground_epoch == self.ink.epoch
            })
            && self
                .ink
                .region
                .is_none_or(|region| point_inside(region, frame.x, frame.y));
        if !valid_lease {
            return Ok(());
        }
        match frame.phase {
            PenPhase::Down => self.begin_pen(frame),
            PenPhase::Move if self.active_pen => self.move_pen(frame),
            PenPhase::Up if self.active_pen => self.end_pen()?,
            PenPhase::Cancel => self.abort_ink(),
            _ => {}
        }
        Ok(())
    }

    fn begin_pen(&mut self, frame: PenFrame) {
        self.active_pen = true;
        self.settle_deadline = None;
        self.settle_started = None;
        self.ink_begin_sequence = self
            .surfaces
            .get(&self.ink.key)
            .map_or(0, |surface| surface.commit_sequence());
        let radius = live_brush_radius(frame.tool, frame.pressure, frame.pressure_max);
        self.last_pen = Some(LivePenPoint {
            x: frame.x,
            y: frame.y,
            radius,
            tool: frame.tool,
        });
        self.draw_live_segment(frame.tool, frame.x, frame.y, frame.x, frame.y, radius);
    }

    fn move_pen(&mut self, frame: PenFrame) {
        let previous = self.last_pen.unwrap_or(LivePenPoint {
            x: frame.x,
            y: frame.y,
            radius: live_brush_radius(frame.tool, frame.pressure, frame.pressure_max),
            tool: frame.tool,
        });
        let desired_radius = live_brush_radius(frame.tool, frame.pressure, frame.pressure_max);
        let radius = live_segment_radius(frame.tool, desired_radius, Some(previous));
        self.draw_live_segment(frame.tool, previous.x, previous.y, frame.x, frame.y, radius);
        self.last_pen = Some(LivePenPoint {
            x: frame.x,
            y: frame.y,
            radius: desired_radius,
            tool: frame.tool,
        });
    }

    fn end_pen(&mut self) -> io::Result<()> {
        // MagicPaper commits points on press/move and treats release only as a
        // stroke boundary. A zero-pressure Up must not add a thin tail.
        self.active_pen = false;
        self.last_pen = None;
        let now = Instant::now();
        self.settle_started = Some(now);
        self.settle_deadline = Some(now + CANONICAL_SETTLE_DELAY);
        self.flush_live(true)
    }

    fn draw_live_segment(
        &mut self,
        tool: PenTool,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        radius: i32,
    ) {
        let Some(surface) = self.surfaces.get(&self.ink.key) else {
            return;
        };
        let geometry = self.geometry_for_logical(surface.width, surface.height);
        let (physical_x0, physical_y0) = geometry.logical_to_physical_point(x0, y0);
        let (physical_x1, physical_y1) = geometry.logical_to_physical_point(x1, y1);
        let color = match tool {
            PenTool::Pen => [0_u8, 0, 0, 0xff],
            PenTool::Eraser => [0xff_u8, 0xff, 0xff, 0xff],
        };
        let stride = self.backend.stride();
        let width = self.backend.width();
        let height = self.backend.height();
        let dirty = draw_line(
            self.backend.pixels_mut(),
            stride,
            width,
            height,
            physical_x0,
            physical_y0,
            physical_x1,
            physical_y1,
            radius,
            color,
        );
        self.live_dirty = self.live_dirty.union(dirty);
        self.canonical_dirty = self
            .canonical_dirty
            .union(Rect::new(x1, y1, 1, 1).expand(radius + 3));
    }

    pub(super) fn flush_deadlines(&mut self) -> io::Result<()> {
        self.flush_live(false)?;
        if !self
            .settle_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Ok(());
        }
        let now = Instant::now();
        let canonical_is_newer = self
            .surfaces
            .get(&self.ink.key)
            .is_some_and(|surface| surface.commit_sequence() > self.ink_begin_sequence);
        let retry_allowed = self
            .settle_started
            .is_some_and(|started| now.duration_since(started) < CANONICAL_SETTLE_LIMIT);
        if !canonical_is_newer && retry_allowed {
            self.settle_deadline = Some(now + CANONICAL_SETTLE_RETRY);
            return Ok(());
        }
        self.settle_deadline = None;
        self.settle_started = None;
        let dirty = std::mem::take(&mut self.canonical_dirty);
        // Never copy an older application buffer over newer live ink.
        if canonical_is_newer && !dirty.is_empty() && self.ink.enabled {
            let lease = PanelLease {
                key: self.ink.key,
                generation: self.ink.generation,
                foreground_epoch: self.ink.epoch,
            };
            self.sync_surface_buffer(lease, dirty)?;
        }
        Ok(())
    }

    pub(super) fn flush_live(&mut self, force: bool) -> io::Result<()> {
        if self.live_dirty.is_empty()
            || (!force && self.last_live_submit.elapsed() < LIVE_SWAP_INTERVAL)
        {
            return Ok(());
        }
        let dirty =
            std::mem::take(&mut self.live_dirty).clip(self.backend.width(), self.backend.height());
        if !dirty.is_empty() {
            let lease = PanelLease {
                key: self.ink.key,
                generation: self.ink.generation,
                foreground_epoch: self.ink.epoch,
            };
            let surface_sequence = self
                .surfaces
                .get(&lease.key)
                .map_or(0, |surface| surface.commit_sequence());
            self.submit(
                dirty,
                RefreshIntent::Ink,
                lease,
                surface_sequence,
                SubmissionReason::LiveInk,
            )?;
            self.last_live_submit = Instant::now();
        }
        Ok(())
    }
}
