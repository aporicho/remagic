use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BacklightSnapshot {
    pub supported: bool,
    #[serde(default)]
    pub percent: Option<u8>,
    #[serde(default)]
    pub forced_off: bool,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub brightness: Option<u32>,
    #[serde(default)]
    pub max_brightness: Option<u32>,
    #[serde(default)]
    pub bl_power: Option<u32>,
    #[serde(default)]
    pub linear_mapping: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl BacklightSnapshot {
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self {
            supported: false,
            percent: None,
            forced_off: false,
            provider: None,
            brightness: None,
            max_brightness: None,
            bl_power: None,
            linear_mapping: None,
            error: Some(message.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_snapshot_has_no_hardware_values() {
        let snapshot = BacklightSnapshot::unsupported("missing");
        assert!(!snapshot.supported);
        assert_eq!(snapshot.percent, None);
        assert_eq!(snapshot.error.as_deref(), Some("missing"));
    }
}
