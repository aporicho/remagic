//! Canonical display and direct-ink wire types. This file intentionally keeps the
//! closely coupled message schema and validation rules at one protocol boundary.

use remagic_core::{AppToken, RuntimeProfile};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PixelFormat {
    Rgb565,
    Xrgb8888,
}

impl PixelFormat {
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgb565 => 2,
            Self::Xrgb8888 => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DamageRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl DamageRect {
    pub fn validate(self) -> Result<(), DisplayValidationError> {
        let right = i64::from(self.x) + i64::from(self.width);
        let bottom = i64::from(self.y) + i64::from(self.height);
        if self.x < 0
            || self.y < 0
            || self.width == 0
            || self.height == 0
            || right > i64::from(i32::MAX)
            || bottom > i64::from(i32::MAX)
        {
            return Err(DisplayValidationError::InvalidDamageRect(self));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SurfaceDescriptor {
    pub surface_id: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub byte_len: u64,
    pub pixel_format: PixelFormat,
}

impl SurfaceDescriptor {
    pub fn validate(&self) -> Result<(), DisplayValidationError> {
        if self.surface_id == 0 || self.width == 0 || self.height == 0 {
            return Err(DisplayValidationError::InvalidSurfaceDimensions);
        }
        let minimum_stride = self
            .width
            .checked_mul(self.pixel_format.bytes_per_pixel())
            .ok_or(DisplayValidationError::SurfaceSizeOverflow)?;
        if self.stride < minimum_stride {
            return Err(DisplayValidationError::InvalidStride {
                minimum: minimum_stride,
                actual: self.stride,
            });
        }
        let minimum_len = u64::from(self.stride)
            .checked_mul(u64::from(self.height))
            .ok_or(DisplayValidationError::SurfaceSizeOverflow)?;
        if self.byte_len < minimum_len {
            return Err(DisplayValidationError::InvalidByteLength {
                minimum: minimum_len,
                actual: self.byte_len,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameIntent {
    Ink,
    Ui,
    Content,
    Full,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FrameCommit {
    pub token: AppToken,
    pub surface_id: u64,
    pub frame_sequence: u64,
    pub damage_rects: Vec<DamageRect>,
    pub intent: FrameIntent,
}

impl FrameCommit {
    pub fn validate(&self) -> Result<(), DisplayValidationError> {
        self.token
            .validate()
            .map_err(|error| DisplayValidationError::Token(error.to_string()))?;
        if self.token.lease_id.is_none() {
            return Err(DisplayValidationError::MissingLease);
        }
        if self.surface_id == 0 || self.frame_sequence == 0 {
            return Err(DisplayValidationError::ZeroSequenceOrSurface);
        }
        validate_damage(&self.damage_rects)
    }
}

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

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PenFrame {
    pub sequence: u64,
    pub kernel_time_ns: u64,
    pub phase: PenPhase,
    pub tool: PenTool,
    pub x: f32,
    pub y: f32,
    pub pressure: f32,
}

impl PenFrame {
    pub fn validate(self) -> Result<(), DisplayValidationError> {
        if self.sequence == 0
            || !self.x.is_finite()
            || !self.y.is_finite()
            || !self.pressure.is_finite()
            || self.x < 0.0
            || self.y < 0.0
            || !(0.0..=1.0).contains(&self.pressure)
        {
            return Err(DisplayValidationError::InvalidPenFrame);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TouchPhase {
    Down,
    Move,
    Up,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TouchFrame {
    pub sequence: u64,
    pub kernel_time_ns: u64,
    pub contact_id: u32,
    pub phase: TouchPhase,
    pub x: f32,
    pub y: f32,
}

impl TouchFrame {
    pub fn validate(self) -> Result<(), DisplayValidationError> {
        if self.sequence == 0
            || !self.x.is_finite()
            || !self.y.is_finite()
            || self.x < 0.0
            || self.y < 0.0
        {
            return Err(DisplayValidationError::InvalidTouchFrame);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InkCommit {
    pub token: AppToken,
    pub stroke_id: u64,
    pub frame_sequence: u64,
    pub damage_rects: Vec<DamageRect>,
}

impl InkCommit {
    pub fn validate(&self) -> Result<(), DisplayValidationError> {
        self.token
            .validate()
            .map_err(|error| DisplayValidationError::Token(error.to_string()))?;
        if self.token.lease_id.is_none() {
            return Err(DisplayValidationError::MissingLease);
        }
        if self.stroke_id == 0 || self.frame_sequence == 0 {
            return Err(DisplayValidationError::ZeroInkIdentifier);
        }
        validate_damage(&self.damage_rects)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InkCancel {
    pub token: AppToken,
    #[serde(default)]
    pub stroke_id: Option<u64>,
    #[serde(default)]
    pub damage_rects: Vec<DamageRect>,
}

impl InkCancel {
    pub fn validate(&self) -> Result<(), DisplayValidationError> {
        self.token
            .validate()
            .map_err(|error| DisplayValidationError::Token(error.to_string()))?;
        if self.token.lease_id.is_none() {
            return Err(DisplayValidationError::MissingLease);
        }
        if self.stroke_id == Some(0) {
            return Err(DisplayValidationError::ZeroInkIdentifier);
        }
        if self.damage_rects.len() > 64 {
            return Err(DisplayValidationError::TooManyDamageRects(
                self.damage_rects.len(),
            ));
        }
        for rect in &self.damage_rects {
            rect.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case")]
pub enum DisplayClientMessage {
    Attach {
        token: AppToken,
        profile: RuntimeProfile,
        preferred_format: PixelFormat,
    },
    FrameCommit {
        commit: FrameCommit,
    },
    InkCommit {
        commit: InkCommit,
    },
    InkCancel {
        cancel: InkCancel,
    },
    Release {
        token: AppToken,
    },
}

impl DisplayClientMessage {
    pub fn validate(&self) -> Result<(), DisplayValidationError> {
        match self {
            Self::Attach { token, .. } => {
                token
                    .validate()
                    .map_err(|error| DisplayValidationError::Token(error.to_string()))?;
                if token.lease_id.is_none() {
                    return Err(DisplayValidationError::MissingLease);
                }
                Ok(())
            }
            Self::FrameCommit { commit } => commit.validate(),
            Self::InkCommit { commit } => commit.validate(),
            Self::InkCancel { cancel } => cancel.validate(),
            Self::Release { token } => token
                .validate()
                .map_err(|error| DisplayValidationError::Token(error.to_string())),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "message", rename_all = "snake_case")]
pub enum DisplayHostMessage {
    Attached {
        token: AppToken,
        surface: SurfaceDescriptor,
    },
    PenFrame {
        token: AppToken,
        frame: PenFrame,
    },
    TouchFrame {
        token: AppToken,
        frame: TouchFrame,
    },
    LeaseRevoked {
        token: AppToken,
        reason: LeaseRevocationReason,
    },
    Error {
        #[serde(default)]
        token: Option<AppToken>,
        code: DisplayErrorCode,
        detail: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseRevocationReason {
    Background,
    AppExit,
    ReturnStock,
    Recovery,
    Replaced,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplayErrorCode {
    StaleToken,
    InvalidSurface,
    InvalidFrame,
    PermissionDenied,
    BackendUnavailable,
    Internal,
}

fn validate_damage(rects: &[DamageRect]) -> Result<(), DisplayValidationError> {
    if rects.is_empty() {
        return Err(DisplayValidationError::MissingDamage);
    }
    if rects.len() > 64 {
        return Err(DisplayValidationError::TooManyDamageRects(rects.len()));
    }
    for rect in rects {
        rect.validate()?;
    }
    Ok(())
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum DisplayValidationError {
    #[error("invalid application token: {0}")]
    Token(String),
    #[error("foreground display message has no lease")]
    MissingLease,
    #[error("invalid damage rectangle {0:?}")]
    InvalidDamageRect(DamageRect),
    #[error("at least one damage rectangle is required")]
    MissingDamage,
    #[error("too many damage rectangles: {0}")]
    TooManyDamageRects(usize),
    #[error("surface id, width, and height must be non-zero")]
    InvalidSurfaceDimensions,
    #[error("surface size arithmetic overflow")]
    SurfaceSizeOverflow,
    #[error("surface stride {actual} is less than minimum {minimum}")]
    InvalidStride { minimum: u32, actual: u32 },
    #[error("surface byte length {actual} is less than minimum {minimum}")]
    InvalidByteLength { minimum: u64, actual: u64 },
    #[error("surface id and frame sequence must be non-zero")]
    ZeroSequenceOrSurface,
    #[error("stroke id and frame sequence must be non-zero")]
    ZeroInkIdentifier,
    #[error("invalid pen frame")]
    InvalidPenFrame,
    #[error("invalid touch frame")]
    InvalidTouchFrame,
}

#[cfg(test)]
mod tests;
