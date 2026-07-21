use serde::{Deserialize, Serialize};

mod decoder;
mod device;

pub use decoder::{MarkerDecoder, RawEvent, TouchDecoder};
pub use device::InputThreads;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const SYN_REPORT: u16 = 0;
const SYN_DROPPED: u16 = 3;
const ABS_X: u16 = 0;
const ABS_Y: u16 = 1;
const ABS_PRESSURE: u16 = 24;
const ABS_MT_SLOT: u16 = 47;
const ABS_MT_POSITION_X: u16 = 53;
const ABS_MT_POSITION_Y: u16 = 54;
const ABS_MT_TRACKING_ID: u16 = 57;
const ABS_MT_PRESSURE: u16 = 58;
const BTN_TOOL_PEN: u16 = 320;
const BTN_TOOL_RUBBER: u16 = 321;
const BTN_TOUCH: u16 = 330;

const EVIOCGRAB: libc::c_ulong = 0x4004_4590;
const EVIOCGABS_X: libc::c_ulong = 0x8018_4540;
const EVIOCGABS_Y: libc::c_ulong = 0x8018_4541;
const EVIOCGABS_PRESSURE: libc::c_ulong = 0x8018_4558;
const EVIOCGABS_MT_SLOT: libc::c_ulong = 0x8018_456f;
const EVIOCGABS_MT_X: libc::c_ulong = 0x8018_4575;
const EVIOCGABS_MT_Y: libc::c_ulong = 0x8018_4576;
const EVIOCGABS_MT_PRESSURE: libc::c_ulong = 0x8018_457a;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PenPhase {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PenTool {
    Pen,
    Eraser,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PenFrame {
    pub sequence: u64,
    pub kernel_time_ns: u64,
    pub phase: PenPhase,
    pub tool: PenTool,
    pub x: i32,
    pub y: i32,
    pub pressure: i32,
    pub pressure_max: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchPhase {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TouchFrame {
    pub sequence: u64,
    pub kernel_time_ns: u64,
    pub phase: TouchPhase,
    pub device_id: i32,
    pub x: i32,
    pub y: i32,
    pub pressure: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputFrame {
    Pen(PenFrame),
    Touch(TouchFrame),
}

#[derive(Clone, Copy, Debug)]
pub struct AxisRange {
    pub minimum: i32,
    pub maximum: i32,
}

impl AxisRange {
    fn scale(self, value: i32, extent: i32) -> i32 {
        if extent <= 1 || self.maximum <= self.minimum {
            return 0;
        }
        let value = value.clamp(self.minimum, self.maximum) - self.minimum;
        let source = self.maximum - self.minimum;
        ((value as i64 * (extent - 1) as i64 + source as i64 / 2) / source as i64) as i32
    }
}

#[cfg(test)]
mod tests;
