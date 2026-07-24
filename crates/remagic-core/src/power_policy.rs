//! System-wide power policy and observable state.
//!
//! These types deliberately describe policy rather than Linux mechanisms.
//! Applications may request bounded work through the runtime protocol, but
//! only the ReMagic supervisor is allowed to translate that request into a
//! wake lock, an RTC alarm, or a process-freezer transition.

use crate::AppId;
use serde::{Deserialize, Serialize};

pub const DEFAULT_IDLE_SUSPEND_SECS: u64 = 120;
pub const MIN_IDLE_SUSPEND_SECS: u64 = 60;
pub const MAX_IDLE_SUSPEND_SECS: u64 = 30 * 60;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerPhase {
    #[default]
    Awake,
    Quiescing,
    Suspended,
    Resuming,
    ExternallyBlocked,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "surface", content = "app_id", rename_all = "snake_case")]
pub enum PresentationState {
    #[default]
    None,
    Home,
    Foreground(AppId),
    Lock,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadState {
    #[default]
    Idle,
    Interactive,
    ScheduledJob,
    Maintenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkClass {
    AgentTurn,
    Ocr,
    FileTransfer,
    PackageTransaction,
    StorageCommit,
    DisplayCommit,
}

impl WorkClass {
    pub const fn maximum_lease_ms(self) -> u64 {
        match self {
            Self::AgentTurn | Self::Ocr => 180_000,
            Self::FileTransfer => 30_000,
            Self::PackageTransaction => 15 * 60_000,
            Self::StorageCommit | Self::DisplayCommit => 10_000,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PowerSettings {
    pub schema: u32,
    /// Zero explicitly disables automatic suspension. The settings UI keeps
    /// this choice visible because it allows the managed wakelock to remain
    /// held indefinitely.
    pub idle_suspend_secs: u64,
}

impl Default for PowerSettings {
    fn default() -> Self {
        Self {
            schema: 1,
            idle_suspend_secs: DEFAULT_IDLE_SUSPEND_SECS,
        }
    }
}

impl PowerSettings {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != 1 {
            return Err(format!("unsupported power settings schema {}", self.schema));
        }
        if self.idle_suspend_secs != 0
            && !(MIN_IDLE_SUSPEND_SECS..=MAX_IDLE_SUSPEND_SECS).contains(&self.idle_suspend_secs)
        {
            return Err(format!(
                "idle suspension must be disabled or between {MIN_IDLE_SUSPEND_SECS} and {MAX_IDLE_SUSPEND_SECS} seconds"
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResourceLease {
    pub id: u64,
    pub owner: AppId,
    pub class: WorkClass,
    pub reason: String,
    pub expires_at_unix_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PowerSnapshot {
    pub phase: PowerPhase,
    pub presentation: PresentationState,
    pub workload: WorkloadState,
    pub idle_suspend_secs: u64,
    #[serde(default)]
    pub idle_deadline_unix_ms: Option<u64>,
    #[serde(default)]
    pub wake_lock_owners: Vec<String>,
    #[serde(default)]
    pub active_leases: Vec<ResourceLease>,
    #[serde(default)]
    pub next_wake_unix_ms: Option<u64>,
    #[serde(default)]
    pub suspend_successes: u64,
    #[serde(default)]
    pub last_wake_reason: Option<String>,
    #[serde(default)]
    pub external_blocker: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_policy_accepts_only_explicit_supported_values() {
        for seconds in [0, 60, 120, 1_800] {
            assert!(PowerSettings {
                idle_suspend_secs: seconds,
                ..PowerSettings::default()
            }
            .validate()
            .is_ok());
        }
        for seconds in [1, 59, 1_801, u64::MAX] {
            assert!(PowerSettings {
                idle_suspend_secs: seconds,
                ..PowerSettings::default()
            }
            .validate()
            .is_err());
        }
    }

    #[test]
    fn every_work_class_has_a_finite_platform_limit() {
        for class in [
            WorkClass::AgentTurn,
            WorkClass::Ocr,
            WorkClass::FileTransfer,
            WorkClass::PackageTransaction,
            WorkClass::StorageCommit,
            WorkClass::DisplayCommit,
        ] {
            assert!(class.maximum_lease_ms() > 0);
            assert!(class.maximum_lease_ms() <= 15 * 60_000);
        }
    }
}
