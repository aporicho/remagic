use super::{Capability, LaunchEnvironment, RuntimeProfile, RuntimeValidationError};
use crate::AppId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightStatus {
    Passed,
    Warning,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreflightCheck {
    pub id: String,
    pub status: PreflightStatus,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreflightReport {
    pub app_id: AppId,
    pub profile: RuntimeProfile,
    pub compatible: bool,
    #[serde(default)]
    pub checks: Vec<PreflightCheck>,
    #[serde(default)]
    pub missing_capabilities: BTreeSet<Capability>,
    #[serde(default)]
    pub missing_libraries: Vec<String>,
    #[serde(default)]
    pub launch_environment: Option<LaunchEnvironment>,
}

impl PreflightReport {
    pub fn validate(&self) -> Result<(), RuntimeValidationError> {
        let mut ids = BTreeSet::new();
        for check in &self.checks {
            if check.id.is_empty()
                || check.id.len() > 64
                || !check.id.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'-' | b'_' | b'.')
                })
            {
                return Err(RuntimeValidationError::InvalidCheckId(check.id.clone()));
            }
            if !ids.insert(&check.id) {
                return Err(RuntimeValidationError::DuplicateCheckId(check.id.clone()));
            }
        }
        let computed_compatible = self
            .checks
            .iter()
            .all(|check| check.status != PreflightStatus::Failed)
            && self.missing_capabilities.is_empty()
            && self.missing_libraries.is_empty();
        if self.compatible != computed_compatible {
            return Err(RuntimeValidationError::IncoherentPreflight);
        }
        if self.compatible {
            let environment = self
                .launch_environment
                .as_ref()
                .ok_or(RuntimeValidationError::MissingLaunchEnvironment)?;
            if environment.app_id != self.app_id || environment.profile != self.profile {
                return Err(RuntimeValidationError::PreflightEnvironmentMismatch);
            }
            environment.validate()?;
        }
        Ok(())
    }
}
