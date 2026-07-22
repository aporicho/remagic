//! ReMagic's clean display host.
//!
//! The host deliberately contains no application lifecycle or package logic.
//! It owns the physical panel and input devices for one managed-domain
//! generation and exposes surfaces, damage and normalized input to clients.

pub mod control;
pub mod geometry;
pub mod input;
pub mod panel;
pub mod protocol;
pub mod qtfb;
pub mod surface;

pub use geometry::{Geometry, Rect};
pub use input::{PenFrame, PenPhase, PenTool, TouchFrame, TouchPhase};
pub use panel::{PanelCommand, RefreshIntent};
