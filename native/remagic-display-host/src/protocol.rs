//! Stable wire definitions shared by the QTFB compatibility server and the
//! native Remagic surface protocol.

use crate::geometry::Rect;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const QTFB_SOCKET: &str = "/tmp/qtfb.sock";
pub const DISPLAY_CONTROL_SOCKET: &str = "/run/remagic/display.sock";
pub const QTFB_CLIENT_MESSAGE_SIZE: usize = 24;
pub const QTFB_SERVER_MESSAGE_SIZE: usize = 32;
pub const QTFB_DEFAULT_KEY: i32 = 245_209_899;

pub const MESSAGE_INITIALIZE: u8 = 0;
pub const MESSAGE_UPDATE: u8 = 1;
pub const MESSAGE_CUSTOM_INITIALIZE: u8 = 2;
pub const MESSAGE_TERMINATE: u8 = 3;
pub const MESSAGE_USERINPUT: u8 = 4;
pub const MESSAGE_SET_REFRESH_MODE: u8 = 5;
pub const MESSAGE_REQUEST_FULL_REFRESH: u8 = 6;

pub const UPDATE_ALL: i32 = 0;
pub const UPDATE_PARTIAL: i32 = 1;

pub const REFRESH_MODE_UFAST: i32 = 0;
pub const REFRESH_MODE_FAST: i32 = 1;
pub const REFRESH_MODE_ANIMATE: i32 = 2;
pub const REFRESH_MODE_CONTENT: i32 = 3;
pub const REFRESH_MODE_UI: i32 = 4;

pub const INPUT_TOUCH_PRESS: i32 = 0x10;
pub const INPUT_TOUCH_RELEASE: i32 = 0x11;
pub const INPUT_TOUCH_UPDATE: i32 = 0x12;
pub const INPUT_PEN_PRESS: i32 = 0x20;
pub const INPUT_PEN_RELEASE: i32 = 0x21;
pub const INPUT_PEN_UPDATE: i32 = 0x22;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Rgb565,
    Rgb888,
    Rgba8888,
}

impl PixelFormat {
    pub fn from_qtfb(value: u8) -> Option<Self> {
        match value {
            0 | 3 | 6 => Some(Self::Rgb565),
            1 | 4 => Some(Self::Rgb888),
            2 | 5 => Some(Self::Rgba8888),
            _ => None,
        }
    }

    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb565 => 2,
            Self::Rgb888 => 3,
            Self::Rgba8888 => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClientPacket {
    Initialize {
        key: i32,
        format: PixelFormat,
        width: i32,
        height: i32,
    },
    Update {
        rect: Option<Rect>,
    },
    Terminate,
    SetRefreshMode(i32),
    RequestFullRefresh,
}

impl ClientPacket {
    pub fn decode(bytes: &[u8; QTFB_CLIENT_MESSAGE_SIZE]) -> Result<Self, &'static str> {
        let i32_at =
            |offset: usize| i32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        match bytes[0] {
            MESSAGE_INITIALIZE => {
                let format = PixelFormat::from_qtfb(bytes[8]).ok_or("unsupported pixel format")?;
                let (width, height) = default_dimensions(bytes[8])?;
                Ok(Self::Initialize {
                    key: i32_at(4),
                    format,
                    width,
                    height,
                })
            }
            MESSAGE_CUSTOM_INITIALIZE => {
                let format = PixelFormat::from_qtfb(bytes[8]).ok_or("unsupported pixel format")?;
                let width = u16::from_le_bytes(bytes[10..12].try_into().unwrap()) as i32;
                let height = u16::from_le_bytes(bytes[12..14].try_into().unwrap()) as i32;
                if width <= 0 || height <= 0 {
                    return Err("invalid custom dimensions");
                }
                Ok(Self::Initialize {
                    key: i32_at(4),
                    format,
                    width,
                    height,
                })
            }
            MESSAGE_UPDATE => match i32_at(4) {
                UPDATE_ALL => Ok(Self::Update { rect: None }),
                UPDATE_PARTIAL => Ok(Self::Update {
                    rect: Some(Rect::new(i32_at(8), i32_at(12), i32_at(16), i32_at(20))),
                }),
                _ => Err("unsupported update type"),
            },
            MESSAGE_TERMINATE => Ok(Self::Terminate),
            MESSAGE_SET_REFRESH_MODE => {
                let mode = i32_at(4);
                if !(REFRESH_MODE_UFAST..=REFRESH_MODE_UI).contains(&mode) {
                    return Err("invalid refresh mode");
                }
                Ok(Self::SetRefreshMode(mode))
            }
            MESSAGE_REQUEST_FULL_REFRESH => Ok(Self::RequestFullRefresh),
            _ => Err("unknown QTFB packet"),
        }
    }
}

fn default_dimensions(format: u8) -> Result<(i32, i32), &'static str> {
    match format {
        0 => Ok((1404, 1872)),
        1..=3 => Ok((1620, 2160)),
        4..=6 => Ok((954, 1696)),
        _ => Err("unsupported pixel format"),
    }
}

pub fn initialize_reply(shm_key: i32, shm_size: usize) -> [u8; QTFB_SERVER_MESSAGE_SIZE] {
    let mut bytes = [0_u8; QTFB_SERVER_MESSAGE_SIZE];
    bytes[0] = MESSAGE_INITIALIZE;
    bytes[8..12].copy_from_slice(&shm_key.to_le_bytes());
    bytes[16..24].copy_from_slice(&(shm_size as u64).to_le_bytes());
    bytes
}

pub fn input_packet(
    input_type: i32,
    device_id: i32,
    x: i32,
    y: i32,
    detail: i32,
) -> [u8; QTFB_SERVER_MESSAGE_SIZE] {
    let mut bytes = [0_u8; QTFB_SERVER_MESSAGE_SIZE];
    bytes[0] = MESSAGE_USERINPUT;
    for (offset, value) in [input_type, device_id, x, y, detail]
        .into_iter()
        .enumerate()
    {
        let start = 8 + offset * 4;
        bytes[start..start + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ControlEnvelope {
    pub protocol: u32,
    pub request_id: String,
    #[serde(flatten)]
    pub command: DisplayControl,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum DisplayControl {
    Status,
    SetForeground {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
        #[serde(default = "default_true")]
        full_refresh: bool,
    },
    PrepareForeground {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
    },
    ActivateForeground {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
        ink_enabled: bool,
        #[serde(default = "default_true")]
        full_refresh: bool,
    },
    ClearForeground,
    ConfigureInk {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
        enabled: bool,
        #[serde(default)]
        region: Option<Rect>,
    },
    RequestFullRefresh,
    ShowLock {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
        sleep_epoch: u64,
        unlock_region: Rect,
    },
    RefreshLock {
        sleep_epoch: u64,
    },
    CancelLock {
        sleep_epoch: u64,
        replacement_surface_sequence: u64,
    },
    InjectTap {
        x: i32,
        y: i32,
    },
    InjectPenLine {
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        #[serde(default = "default_pen_points")]
        points: u16,
    },
    Shutdown,
}

fn default_true() -> bool {
    true
}

fn default_pen_points() -> u16 {
    16
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ControlReply {
    pub protocol: u32,
    pub request_id: String,
    pub ok: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<DisplaySnapshot>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct DisplaySnapshot {
    pub physical_width: i32,
    pub physical_height: i32,
    pub stride: usize,
    pub surfaces: Vec<i32>,
    #[serde(default)]
    pub surface_sequences: BTreeMap<i32, u64>,
    #[serde(default)]
    pub surface_signatures: BTreeMap<i32, u64>,
    pub foreground_key: Option<i32>,
    pub generation: u64,
    pub foreground_epoch: u64,
    pub ink_enabled: bool,
    #[serde(default)]
    pub lock_epoch: u64,
    #[serde(default)]
    pub lock_committed: bool,
    pub queue_depth: usize,
    pub input_backpressure_events: u64,
    pub panel_submission_count: u64,
    pub panel_last_marker: u64,
    pub panel_failure_count: u64,
    pub visible_signature: u64,
    #[serde(default)]
    pub full_refresh_count: u64,
    #[serde(default)]
    pub last_presented_key: Option<i32>,
    #[serde(default)]
    pub last_presented_sequence: u64,
    /// Fixed-capacity, oldest-to-newest evidence from the panel consumer.
    /// Unlike aggregate counters these records retain the exact foreground
    /// fence that was validated for each attempted hardware submission.
    #[serde(default)]
    pub recent_submissions: Vec<crate::panel::SubmissionRecord>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn move_initialize_packet_decodes_with_known_layout() {
        let mut bytes = [0_u8; QTFB_CLIENT_MESSAGE_SIZE];
        bytes[0] = MESSAGE_INITIALIZE;
        bytes[4..8].copy_from_slice(&123_i32.to_le_bytes());
        bytes[8] = 6;
        assert_eq!(
            ClientPacket::decode(&bytes),
            Ok(ClientPacket::Initialize {
                key: 123,
                format: PixelFormat::Rgb565,
                width: 954,
                height: 1696,
            })
        );
    }

    #[test]
    fn partial_update_uses_all_twenty_four_bytes() {
        let mut bytes = [0_u8; QTFB_CLIENT_MESSAGE_SIZE];
        bytes[0] = MESSAGE_UPDATE;
        bytes[4..8].copy_from_slice(&UPDATE_PARTIAL.to_le_bytes());
        for (offset, value) in [(8, 1_i32), (12, 2), (16, 30), (20, 40)] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        assert_eq!(
            ClientPacket::decode(&bytes),
            Ok(ClientPacket::Update {
                rect: Some(Rect::new(1, 2, 30, 40))
            })
        );
    }

    #[test]
    fn server_packets_match_the_legacy_offsets() {
        let init = initialize_reply(7, 99);
        assert_eq!(i32::from_le_bytes(init[8..12].try_into().unwrap()), 7);
        assert_eq!(u64::from_le_bytes(init[16..24].try_into().unwrap()), 99);

        let input = input_packet(INPUT_PEN_UPDATE, 1, 3, 4, 75);
        assert_eq!(i32::from_le_bytes(input[8..12].try_into().unwrap()), 0x22);
        assert_eq!(i32::from_le_bytes(input[24..28].try_into().unwrap()), 75);
    }

    #[test]
    fn control_envelope_round_trips_epoch_fence() {
        let request = ControlEnvelope {
            protocol: 1,
            request_id: "test-1".into(),
            command: DisplayControl::SetForeground {
                key: 9,
                generation: 3,
                foreground_epoch: 11,
                full_refresh: true,
            },
        };
        let encoded = serde_json::to_vec(&request).unwrap();
        let decoded: ControlEnvelope = serde_json::from_slice(&encoded).unwrap();
        assert!(matches!(
            decoded.command,
            DisplayControl::SetForeground {
                key: 9,
                generation: 3,
                foreground_epoch: 11,
                full_refresh: true,
            }
        ));
    }

    #[test]
    fn configure_ink_control_round_trips_the_full_foreground_fence() {
        let request = ControlEnvelope {
            protocol: 1,
            request_id: "ink-mode-1".into(),
            command: DisplayControl::ConfigureInk {
                key: 19,
                generation: 7,
                foreground_epoch: 13,
                enabled: false,
                region: None,
            },
        };
        let encoded = serde_json::to_vec(&request).unwrap();
        let decoded: ControlEnvelope = serde_json::from_slice(&encoded).unwrap();
        assert!(matches!(
            decoded.command,
            DisplayControl::ConfigureInk {
                key: 19,
                generation: 7,
                foreground_epoch: 13,
                enabled: false,
                region: None,
            }
        ));
    }

    #[test]
    fn lock_control_round_trips_epoch_and_unlock_region() {
        let request = ControlEnvelope {
            protocol: 1,
            request_id: "lock-1".into(),
            command: DisplayControl::ShowLock {
                key: 19,
                generation: 8,
                foreground_epoch: 14,
                sleep_epoch: 3,
                unlock_region: Rect::new(150, 1010, 654, 126),
            },
        };
        let decoded: ControlEnvelope =
            serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
        assert!(matches!(
            decoded.command,
            DisplayControl::ShowLock {
                key: 19,
                generation: 8,
                foreground_epoch: 14,
                sleep_epoch: 3,
                unlock_region,
            } if unlock_region == Rect::new(150, 1010, 654, 126)
        ));
    }

    #[test]
    fn atomic_foreground_activation_round_trips_ink_policy() {
        let request = ControlEnvelope {
            protocol: 1,
            request_id: "activate-1".into(),
            command: DisplayControl::ActivateForeground {
                key: 29,
                generation: 18,
                foreground_epoch: 24,
                ink_enabled: true,
                full_refresh: true,
            },
        };
        let decoded: ControlEnvelope =
            serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
        assert!(matches!(
            decoded.command,
            DisplayControl::ActivateForeground {
                key: 29,
                generation: 18,
                foreground_epoch: 24,
                ink_enabled: true,
                full_refresh: true,
            }
        ));
    }
}
