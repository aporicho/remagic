use super::queue::InputPush;
use super::state::{HostState, LockLease};
use crate::input::{
    CapturedInput, InputFrame, PenFrame, PenPhase, PenTool, TouchFrame, TouchPhase,
};
use crate::panel::PanelCommand;
use crate::protocol::{
    input_packet, INPUT_PEN_PRESS, INPUT_PEN_RELEASE, INPUT_PEN_UPDATE, INPUT_TOUCH_PRESS,
    INPUT_TOUCH_RELEASE, INPUT_TOUCH_UPDATE, QTFB_SERVER_MESSAGE_SIZE,
};
use std::io;
use std::sync::atomic::Ordering;

impl HostState {
    pub fn dispatch_input(&self, frame: InputFrame) -> io::Result<()> {
        let captured = CapturedInput {
            epoch: self.input_epoch.load(Ordering::Acquire),
            frame,
        };
        self.dispatch_captured_input(captured)
    }

    pub fn dispatch_captured_input(&self, captured: CapturedInput) -> io::Result<()> {
        // Serialize input routing with Set/ClearForeground. A committed lease
        // alone is insufficient during a transition: once a switch has been
        // requested, the previously visible client must stop receiving new
        // events even if the panel worker has not committed the next frame
        // yet. Keeping this operation guard through enqueue/send also gives
        // the panel command channel a deterministic Pen-before-Set or
        // Set-before-Pen order.
        let _operation = self.foreground_ops.lock().unwrap();
        if captured.epoch != self.input_epoch.load(Ordering::Acquire) {
            return Err(input_not_routed("stale input epoch"));
        }
        let frame = captured.frame;
        if self.suppress_fenced_contact(frame) {
            return Err(input_not_routed(
                "contact belongs to an old foreground fence",
            ));
        }
        if self.prepared_foreground.lock().unwrap().is_some() {
            return Err(input_not_routed("foreground transition is still prepared"));
        }
        let lock = *self.lock.lock().unwrap();
        if lock.is_some_and(|lock| !self.route_lock_contact(frame, lock)) {
            return Err(input_not_routed(
                "contact is outside the lock-screen control",
            ));
        }
        let requested = self
            .foreground
            .lock()
            .unwrap()
            .map(|foreground| foreground.panel_lease());
        let Some(lease) = self.telemetry.committed_foreground() else {
            return Err(input_not_routed("panel has no committed foreground"));
        };
        if requested != Some(lease) {
            return Err(input_not_routed(
                "requested and panel-committed foregrounds differ",
            ));
        }
        if lock.is_none() && !self.route_foreground_touch(frame) {
            return Err(input_not_routed("touch sequence has no active contact"));
        }
        if !self.track_pen_contact(frame, lease) {
            return Err(input_not_routed("pen sequence has no active contact"));
        }
        let packet = match frame {
            InputFrame::Pen(frame) => {
                let packet = self.pen_packet(frame);
                let _ = self.enqueue_panel(PanelCommand::Pen { lease, frame });
                packet
            }
            InputFrame::Touch(frame) => touch_packet(frame),
        };
        self.deliver_input(lease.key, &packet)
    }

    fn deliver_input(&self, key: i32, packet: &[u8; QTFB_SERVER_MESSAGE_SIZE]) -> io::Result<()> {
        self.send_to_key(key, packet)
            .then_some(())
            .ok_or_else(|| input_not_routed("foreground surface has no live input client"))
    }

    fn suppress_fenced_contact(&self, frame: InputFrame) -> bool {
        if let InputFrame::Touch(touch) = frame {
            let mut suppressed = self.suppressed_touches.lock().unwrap();
            if suppressed.contains(&touch.device_id) {
                if matches!(touch.phase, TouchPhase::Up | TouchPhase::Cancel) {
                    suppressed.remove(&touch.device_id);
                }
                return true;
            }
        }
        if let InputFrame::Pen(pen) = frame {
            if self.suppressed_pen.load(Ordering::Acquire) {
                return match pen.phase {
                    PenPhase::Down => {
                        self.suppressed_pen.store(false, Ordering::Release);
                        false
                    }
                    PenPhase::Up | PenPhase::Cancel => {
                        self.suppressed_pen.store(false, Ordering::Release);
                        true
                    }
                    PenPhase::Move => true,
                };
            }
        }
        false
    }

    /// A locked domain only routes an unlock gesture that began inside the
    /// explicit unlock control. Pen input stays fenced until cancellation.
    fn route_lock_contact(&self, frame: InputFrame, lock: LockLease) -> bool {
        let InputFrame::Touch(touch) = frame else {
            return false;
        };
        let mut active = self.lock_touches.lock().unwrap();
        let in_unlock_region = touch.x >= lock.unlock_region.x
            && touch.x < lock.unlock_region.right()
            && touch.y >= lock.unlock_region.y
            && touch.y < lock.unlock_region.bottom();
        match touch.phase {
            TouchPhase::Down if in_unlock_region => {
                active.insert(touch.device_id);
                true
            }
            TouchPhase::Move => active.contains(&touch.device_id),
            TouchPhase::Up | TouchPhase::Cancel => active.remove(&touch.device_id),
            TouchPhase::Down => false,
        }
    }

    fn route_foreground_touch(&self, frame: InputFrame) -> bool {
        let InputFrame::Touch(touch) = frame else {
            return true;
        };
        let mut active = self.active_touches.lock().unwrap();
        match touch.phase {
            TouchPhase::Down => {
                active.insert(touch.device_id);
                true
            }
            TouchPhase::Move => active.contains(&touch.device_id),
            TouchPhase::Up | TouchPhase::Cancel => active.remove(&touch.device_id),
        }
    }

    fn track_pen_contact(&self, frame: InputFrame, lease: crate::panel::PanelLease) -> bool {
        let InputFrame::Pen(pen) = frame else {
            return true;
        };
        let mut active = self.active_pen.lock().unwrap();
        match pen.phase {
            PenPhase::Down => {
                if let Some((previous_lease, previous)) = active.take() {
                    let cancelled = PenFrame {
                        phase: PenPhase::Cancel,
                        pressure: 0,
                        ..previous
                    };
                    self.send_to_key(previous_lease.key, &self.pen_packet(cancelled));
                    let _ = self.enqueue_panel(PanelCommand::Pen {
                        lease: previous_lease,
                        frame: cancelled,
                    });
                }
                *active = Some((lease, pen));
                true
            }
            PenPhase::Move => {
                let Some((owner, last)) = active.as_mut() else {
                    return false;
                };
                if *owner != lease {
                    return false;
                }
                *last = pen;
                true
            }
            PenPhase::Up | PenPhase::Cancel => {
                active.take().is_some_and(|(owner, _)| owner == lease)
            }
        }
    }

    pub(super) fn pen_packet(&self, frame: PenFrame) -> [u8; QTFB_SERVER_MESSAGE_SIZE] {
        let input_type = match frame.phase {
            PenPhase::Down => INPUT_PEN_PRESS,
            PenPhase::Move => INPUT_PEN_UPDATE,
            PenPhase::Up | PenPhase::Cancel => INPUT_PEN_RELEASE,
        };
        let device = i32::from(frame.tool == PenTool::Eraser);
        let pressure = frame.pressure.saturating_mul(100) / frame.pressure_max.max(1);
        input_packet(input_type, device, frame.x, frame.y, pressure)
    }

    pub fn inject_tap(&self, x: i32, y: i32) -> io::Result<()> {
        self.validate_point(x, y)?;
        let sequence = self.injected_sequence.fetch_add(2, Ordering::Relaxed);
        for (offset, phase, pressure) in [(0, TouchPhase::Down, 64), (1, TouchPhase::Up, 0)] {
            self.dispatch_input(InputFrame::Touch(TouchFrame {
                sequence: sequence + offset,
                kernel_time_ns: 0,
                phase,
                device_id: 10_000,
                x,
                y,
                pressure,
            }))
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("injected touch {phase:?} was not routed: {error}"),
                )
            })?;
            if phase == TouchPhase::Down {
                // The control API models a real finger tap for device
                // acceptance. Leave the pressed frame visible long enough for
                // the client and panel worker to present it before release.
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        }
        Ok(())
    }

    pub fn inject_pen_line(
        &self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        points: u16,
    ) -> io::Result<()> {
        self.validate_point(x0, y0)?;
        self.validate_point(x1, y1)?;
        if !(2..=256).contains(&points) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "pen line points must be between 2 and 256",
            ));
        }
        let base = self
            .injected_sequence
            .fetch_add(u64::from(points) + 1, Ordering::Relaxed);
        for index in 0..points {
            self.dispatch_input(InputFrame::Pen(interpolated_pen_frame(
                base, index, points, x0, y0, x1, y1,
            )))?;
        }
        self.dispatch_input(InputFrame::Pen(PenFrame {
            sequence: base + u64::from(points),
            kernel_time_ns: 0,
            phase: PenPhase::Up,
            tool: PenTool::Pen,
            x: x1,
            y: y1,
            pressure: 0,
            pressure_max: 4096,
        }))?;
        Ok(())
    }

    fn validate_point(&self, x: i32, y: i32) -> io::Result<()> {
        let foreground = self.foreground.lock().unwrap().map(|lease| lease.key);
        let surfaces = self.surfaces.lock().unwrap();
        let (width, height) = foreground
            .and_then(|key| surfaces.get(&key))
            .map(|entry| (entry.surface.width, entry.surface.height))
            .unwrap_or((self.physical_width, self.physical_height));
        if (0..width).contains(&x) && (0..height).contains(&y) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("input point {x},{y} is outside the {width}x{height} logical display"),
            ))
        }
    }

    pub(super) fn send_to_key(&self, key: i32, packet: &[u8; QTFB_SERVER_MESSAGE_SIZE]) -> bool {
        let mut surfaces = self.surfaces.lock().unwrap();
        let Some(entry) = surfaces.get_mut(&key) else {
            return false;
        };
        let mut delivered = false;
        entry.clients.retain(|sink| match sink.queue.push(*packet) {
            InputPush::Queued => {
                delivered = true;
                true
            }
            InputPush::Coalesced => {
                self.input_backpressure.fetch_add(1, Ordering::Relaxed);
                delivered = true;
                true
            }
            InputPush::Closed => false,
            InputPush::BoundaryOverflow => {
                self.input_backpressure.fetch_add(1, Ordering::Relaxed);
                false
            }
        });
        delivered
    }
}

fn input_not_routed(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::WouldBlock, reason)
}

fn touch_packet(frame: TouchFrame) -> [u8; QTFB_SERVER_MESSAGE_SIZE] {
    let input_type = match frame.phase {
        TouchPhase::Down => INPUT_TOUCH_PRESS,
        TouchPhase::Move => INPUT_TOUCH_UPDATE,
        TouchPhase::Up | TouchPhase::Cancel => INPUT_TOUCH_RELEASE,
    };
    input_packet(
        input_type,
        frame.device_id,
        frame.x,
        frame.y,
        frame.pressure,
    )
}

#[allow(clippy::too_many_arguments)]
fn interpolated_pen_frame(
    base: u64,
    index: u16,
    points: u16,
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
) -> PenFrame {
    let denominator = i64::from(points - 1);
    let x = i64::from(x0) + (i64::from(x1 - x0) * i64::from(index)) / denominator;
    let y = i64::from(y0) + (i64::from(y1 - y0) * i64::from(index)) / denominator;
    PenFrame {
        sequence: base + u64::from(index),
        kernel_time_ns: 0,
        phase: if index == 0 {
            PenPhase::Down
        } else {
            PenPhase::Move
        },
        tool: PenTool::Pen,
        x: x as i32,
        y: y as i32,
        pressure: 2048,
        pressure_max: 4096,
    }
}
