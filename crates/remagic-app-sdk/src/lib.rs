//! Stable building blocks for native ReMagic applications.
//!
//! The SDK owns the platform boundary: managed launch environment, lifecycle
//! fencing, the QTFB v1 compatibility surface, and safe RGB565 drawing. Apps
//! should keep product behaviour outside this crate.

mod environment;
mod lifecycle;
mod qtfb;
mod surface;

pub use environment::{ManagedEnvironment, ManagedEnvironmentError};
pub use lifecycle::{LifecycleClient, LifecycleError};
pub use qtfb::{InputEvent, QtfbClient, QtfbError, TouchPhase, REFRESH_FAST, REFRESH_UI};
pub use remagic_protocol::{LifecycleCommand, LifecycleStage, ShutdownReason};
pub use surface::{Rgb565, Surface, BLACK, WHITE};
