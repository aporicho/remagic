//! Shared model and persistence for Remagic Manager.

pub mod manifest;
pub mod power;
pub mod session;
pub mod state;

pub use manifest::{AppId, AppManifest, ManifestStore, ParkStrategy};
pub use session::{AppSession, SessionStatus, SessionStore};
pub use state::{DomainState, ManagerState, Transition, TransitionError};

pub const SYSTEM_APP_ID: &str = "system";
pub const HOME_APP_ID: &str = "remagic-home";
