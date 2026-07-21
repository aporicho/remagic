use crate::AppId;
use serde::{Deserialize, Serialize};
use std::fmt;

mod environment;
mod error;
mod policy;
mod preflight;

pub use environment::{
    is_platform_reserved_environment, validate_environment_pair, LaunchEnvironment,
};
pub use error::RuntimeValidationError;
pub use policy::{
    CertificatePolicy, FontPolicy, LocalePolicy, NetworkEnforcement, NetworkMode, NetworkPolicy,
    RuntimeDirectories, RuntimeRequirements, TimezonePolicy,
};
pub use preflight::{PreflightCheck, PreflightReport, PreflightStatus};

/// Reserved QTFB key for the manager home surface.
pub const REMAGIC_HOME_QTFB_KEY: i32 = 245_209_900;

/// Deterministically assigns a positive QTFB key to an application.
///
/// FNV-1a is used intentionally rather than Rust's randomized `Hash`: the same
/// manifest must receive the same key across processes, reboots, and releases.
/// The home key is skipped because it belongs exclusively to remagic-home.
pub fn qtfb_key_for_app(app_id: &AppId) -> i32 {
    let mut hash = 0x811c_9dc5_u32;
    for byte in b"remagic-qtfb-v2:".iter().chain(app_id.as_str().as_bytes()) {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    let mut key = (hash & 0x7fff_ffff).max(1) as i32;
    if key == REMAGIC_HOME_QTFB_KEY {
        key = if key == i32::MAX { 1 } else { key + 1 };
    }
    key
}

/// A capability supplied by the Remagic platform to an application.
///
/// Capabilities are deliberately namespaced strings rather than a closed enum:
/// the manager must be able to reject an unknown requirement at preflight time
/// without requiring every application to be rebuilt when a capability is
/// added. Examples are `display:surface-v2`, `display:qtfb-v1`, and
/// `ink:direct-v1`.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Capability(String);

impl Capability {
    pub fn new(value: impl Into<String>) -> Result<Self, RuntimeValidationError> {
        let value = value.into();
        let bytes = value.as_bytes();
        let valid = !bytes.is_empty()
            && bytes.len() <= 96
            && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.iter().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'_' | b'.' | b':')
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(RuntimeValidationError::InvalidCapability(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for Capability {
    type Error = RuntimeValidationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Capability> for String {
    fn from(value: Capability) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProfile {
    /// Native Surface v2 client with the platform lifecycle and input APIs.
    #[default]
    NativeV2,
    /// Compatibility application hosted through the QTFB v1 bridge.
    QtfbCompat,
    /// No foreground surface; suitable for a supervised background service.
    Headless,
}

impl RuntimeProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NativeV2 => "native_v2",
            Self::QtfbCompat => "qtfb_compat",
            Self::Headless => "headless",
        }
    }
}

#[cfg(test)]
mod tests;
