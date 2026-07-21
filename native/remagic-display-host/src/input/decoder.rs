use super::{
    AxisRange, PenFrame, PenPhase, PenTool, TouchFrame, TouchPhase, ABS_MT_POSITION_X,
    ABS_MT_POSITION_Y, ABS_MT_PRESSURE, ABS_MT_SLOT, ABS_MT_TRACKING_ID, ABS_PRESSURE, ABS_X,
    ABS_Y, BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, EV_ABS, EV_KEY, EV_SYN, SYN_DROPPED,
    SYN_REPORT,
};

#[derive(Clone, Copy, Debug)]
pub struct RawEvent {
    pub time_ns: u64,
    pub event_type: u16,
    pub code: u16,
    pub value: i32,
}

#[derive(Debug)]
pub struct MarkerDecoder {
    logical_width: i32,
    logical_height: i32,
    x_range: AxisRange,
    y_range: AxisRange,
    pressure_range: AxisRange,
    x: i32,
    y: i32,
    pressure: i32,
    touching: bool,
    reported_down: bool,
    tool: PenTool,
    dirty: bool,
    sequence: u64,
    last_time_ns: u64,
}

impl MarkerDecoder {
    pub fn new(
        logical_width: i32,
        logical_height: i32,
        x_range: AxisRange,
        y_range: AxisRange,
        pressure_range: AxisRange,
    ) -> Self {
        Self {
            logical_width,
            logical_height,
            x_range,
            y_range,
            pressure_range,
            x: 0,
            y: 0,
            pressure: 0,
            touching: false,
            reported_down: false,
            tool: PenTool::Pen,
            dirty: false,
            sequence: 0,
            last_time_ns: 0,
        }
    }

    pub fn consume(&mut self, event: RawEvent) -> Option<PenFrame> {
        self.last_time_ns = event.time_ns;
        if event.event_type == EV_SYN && event.code == SYN_DROPPED {
            self.dirty = false;
            if self.reported_down {
                self.reported_down = false;
                self.touching = false;
                return Some(self.frame(PenPhase::Cancel));
            }
            return None;
        }
        match (event.event_type, event.code) {
            (EV_ABS, ABS_X) => {
                self.x = self.x_range.scale(event.value, self.logical_width);
                self.dirty = true;
            }
            (EV_ABS, ABS_Y) => {
                self.y = self.y_range.scale(event.value, self.logical_height);
                self.dirty = true;
            }
            (EV_ABS, ABS_PRESSURE) => {
                self.pressure = event.value.clamp(0, self.pressure_range.maximum.max(1));
                self.dirty = true;
            }
            (EV_KEY, BTN_TOUCH) => {
                self.touching = event.value != 0;
                self.dirty = true;
            }
            (EV_KEY, BTN_TOOL_RUBBER) if event.value != 0 => {
                self.tool = PenTool::Eraser;
                self.dirty = true;
            }
            (EV_KEY, BTN_TOOL_PEN) if event.value != 0 => {
                self.tool = PenTool::Pen;
                self.dirty = true;
            }
            (EV_SYN, SYN_REPORT) if self.dirty => {
                self.dirty = false;
                let phase = match (self.reported_down, self.touching) {
                    (false, true) => PenPhase::Down,
                    (true, true) => PenPhase::Move,
                    (true, false) => PenPhase::Up,
                    (false, false) => return None,
                };
                self.reported_down = self.touching;
                return Some(self.frame(phase));
            }
            _ => {}
        }
        None
    }

    fn frame(&mut self, phase: PenPhase) -> PenFrame {
        self.sequence = self.sequence.wrapping_add(1).max(1);
        PenFrame {
            sequence: self.sequence,
            kernel_time_ns: self.last_time_ns,
            phase,
            tool: self.tool,
            x: self.x,
            y: self.y,
            pressure: self.pressure,
            pressure_max: self.pressure_range.maximum.max(1),
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct TouchSlot {
    tracking_id: i32,
    reported_id: i32,
    x: i32,
    y: i32,
    pressure: i32,
    dirty: bool,
}

#[derive(Debug)]
pub struct TouchDecoder {
    logical_width: i32,
    logical_height: i32,
    x_range: AxisRange,
    y_range: AxisRange,
    slot: usize,
    slots: Vec<TouchSlot>,
    sequence: u64,
    last_time_ns: u64,
}

impl TouchDecoder {
    pub fn new(
        logical_width: i32,
        logical_height: i32,
        x_range: AxisRange,
        y_range: AxisRange,
        slot_count: usize,
    ) -> Self {
        let mut slots = vec![TouchSlot::default(); slot_count.max(1)];
        for slot in &mut slots {
            slot.tracking_id = -1;
            slot.reported_id = -1;
        }
        Self {
            logical_width,
            logical_height,
            x_range,
            y_range,
            slot: 0,
            slots,
            sequence: 0,
            last_time_ns: 0,
        }
    }

    pub fn consume(&mut self, event: RawEvent) -> Vec<TouchFrame> {
        self.last_time_ns = event.time_ns;
        if event.event_type == EV_SYN && event.code == SYN_DROPPED {
            return self.cancel_all();
        }
        match (event.event_type, event.code) {
            (EV_ABS, ABS_MT_SLOT) => {
                self.slot = (event.value.max(0) as usize).min(self.slots.len() - 1);
            }
            (EV_ABS, ABS_MT_TRACKING_ID) => {
                let slot = &mut self.slots[self.slot];
                slot.tracking_id = event.value;
                slot.dirty = true;
            }
            (EV_ABS, ABS_MT_POSITION_X) => {
                let slot = &mut self.slots[self.slot];
                slot.x = self.x_range.scale(event.value, self.logical_width);
                slot.dirty = true;
            }
            (EV_ABS, ABS_MT_POSITION_Y) => {
                let slot = &mut self.slots[self.slot];
                slot.y = self.y_range.scale(event.value, self.logical_height);
                slot.dirty = true;
            }
            (EV_ABS, ABS_MT_PRESSURE) => {
                let slot = &mut self.slots[self.slot];
                slot.pressure = event.value.clamp(0, 255);
                slot.dirty = true;
            }
            (EV_SYN, SYN_REPORT) => return self.flush(),
            _ => {}
        }
        Vec::new()
    }

    fn flush(&mut self) -> Vec<TouchFrame> {
        let mut frames = Vec::new();
        for index in 0..self.slots.len() {
            let slot = &mut self.slots[index];
            if !slot.dirty {
                continue;
            }
            slot.dirty = false;
            let phase = match (slot.reported_id >= 0, slot.tracking_id >= 0) {
                (false, true) => TouchPhase::Down,
                (true, true) => TouchPhase::Move,
                (true, false) => TouchPhase::Up,
                (false, false) => continue,
            };
            let device_id = if slot.tracking_id >= 0 {
                slot.tracking_id
            } else {
                slot.reported_id
            };
            slot.reported_id = slot.tracking_id;
            self.sequence = self.sequence.wrapping_add(1).max(1);
            frames.push(TouchFrame {
                sequence: self.sequence,
                kernel_time_ns: self.last_time_ns,
                phase,
                device_id,
                x: slot.x,
                y: slot.y,
                pressure: slot.pressure,
            });
        }
        frames
    }

    fn cancel_all(&mut self) -> Vec<TouchFrame> {
        let mut frames = Vec::new();
        for slot in &mut self.slots {
            if slot.reported_id < 0 {
                continue;
            }
            self.sequence = self.sequence.wrapping_add(1).max(1);
            frames.push(TouchFrame {
                sequence: self.sequence,
                kernel_time_ns: self.last_time_ns,
                phase: TouchPhase::Cancel,
                device_id: slot.reported_id,
                x: slot.x,
                y: slot.y,
                pressure: slot.pressure,
            });
            slot.tracking_id = -1;
            slot.reported_id = -1;
            slot.dirty = false;
        }
        frames
    }
}
