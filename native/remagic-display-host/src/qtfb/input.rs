use super::queue::InputPush;
use super::state::HostState;
use crate::input::{InputFrame, PenFrame, PenPhase, PenTool, TouchFrame, TouchPhase};
use crate::panel::PanelCommand;
use crate::protocol::{
    input_packet, INPUT_PEN_PRESS, INPUT_PEN_RELEASE, INPUT_PEN_UPDATE, INPUT_TOUCH_PRESS,
    INPUT_TOUCH_RELEASE, INPUT_TOUCH_UPDATE, QTFB_SERVER_MESSAGE_SIZE,
};
use std::io;
use std::sync::atomic::Ordering;

impl HostState {
    pub fn dispatch_input(&self, frame: InputFrame) {
        // Serialize input routing with Set/ClearForeground. A committed lease
        // alone is insufficient during a transition: once a switch has been
        // requested, the previously visible client must stop receiving new
        // events even if the panel worker has not committed the next frame
        // yet. Keeping this operation guard through enqueue/send also gives
        // the panel command channel a deterministic Pen-before-Set or
        // Set-before-Pen order.
        let _operation = self.foreground_ops.lock().unwrap();
        let requested = self
            .foreground
            .lock()
            .unwrap()
            .map(|foreground| foreground.panel_lease());
        let Some(lease) = self.telemetry.committed_foreground() else {
            return;
        };
        if requested != Some(lease) {
            return;
        }
        let packet = match frame {
            InputFrame::Pen(frame) => {
                let packet = self.pen_packet(frame);
                let _ = self.enqueue_panel(PanelCommand::Pen { lease, frame });
                packet
            }
            InputFrame::Touch(frame) => touch_packet(frame),
        };
        self.send_to_key(lease.key, &packet);
    }

    fn pen_packet(&self, frame: PenFrame) -> [u8; QTFB_SERVER_MESSAGE_SIZE] {
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
            }));
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
            )));
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
        }));
        Ok(())
    }

    fn validate_point(&self, x: i32, y: i32) -> io::Result<()> {
        if (0..954).contains(&x) && (0..1696).contains(&y) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("input point {x},{y} is outside the Move logical display"),
            ))
        }
    }

    fn send_to_key(&self, key: i32, packet: &[u8; QTFB_SERVER_MESSAGE_SIZE]) {
        let mut surfaces = self.surfaces.lock().unwrap();
        let Some(entry) = surfaces.get_mut(&key) else {
            return;
        };
        entry.clients.retain(|sink| match sink.queue.push(*packet) {
            InputPush::Queued => true,
            InputPush::Coalesced => {
                self.input_backpressure.fetch_add(1, Ordering::Relaxed);
                true
            }
            InputPush::Closed => false,
            InputPush::BoundaryOverflow => {
                self.input_backpressure.fetch_add(1, Ordering::Relaxed);
                false
            }
        });
    }
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
