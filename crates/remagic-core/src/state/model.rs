use crate::AppId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainState {
    System,
    EnteringManaged,
    Manager,
    Launching(AppId),
    Foreground(AppId),
    Parking(AppId),
    RestoringSystem,
    Sleeping,
    Recovering,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Transition {
    TriplePower,
    SinglePower,
    Launch(AppId),
    AppReady(AppId),
    AppParked(AppId),
    /// A park attempt failed after the application had been asked to enter the
    /// background. The supervisor proved a new foreground epoch/lease and may
    /// therefore return ownership to the same application process.
    AppRestored(AppId),
    AppExited(AppId),
    AppCrashed(AppId),
    ManagedReady,
    SystemReady,
    Sleep,
    Wake,
    Failure,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagerState {
    pub domain: DomainState,
    pub last_app: Option<AppId>,
    pub sequence: u64,
}

impl Default for ManagerState {
    fn default() -> Self {
        Self {
            domain: DomainState::System,
            last_app: None,
            sequence: 0,
        }
    }
}

/// State of the stock/managed ownership boundary. Application lifecycle is
/// intentionally not encoded in this enum.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemDomainState {
    #[default]
    Stock,
    EnteringManaged,
    Managed,
    LeavingManaged,
    Recovering,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppInstanceState {
    Starting,
    Foreground,
    Background,
    Stopping,
    Exited,
    Crashed,
    Unresponsive,
}

impl AppInstanceState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Exited | Self::Crashed)
    }
}

/// Identity fence attached to every lifecycle, display, input, and ink
/// message. Generation prevents an old process from impersonating its
/// replacement; epoch/lease fence stale foreground work from the same process.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppToken {
    pub app_id: AppId,
    pub generation: u64,
    pub foreground_epoch: u64,
    #[serde(default)]
    pub lease_id: Option<u64>,
}

impl AppToken {
    pub fn validate(&self) -> Result<(), StateModelError> {
        if self.generation == 0 {
            return Err(StateModelError::ZeroGeneration(self.app_id.clone()));
        }
        if self.lease_id == Some(0) {
            return Err(StateModelError::ZeroLease(self.app_id.clone()));
        }
        Ok(())
    }

    pub fn same_process(&self, app_id: &AppId, generation: u64) -> bool {
        &self.app_id == app_id && self.generation == generation
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppInstance {
    pub token: AppToken,
    pub state: AppInstanceState,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl AppInstance {
    pub fn validate(&self) -> Result<(), StateModelError> {
        self.token.validate()?;
        if self.state == AppInstanceState::Foreground && self.token.lease_id.is_none() {
            return Err(StateModelError::ForegroundWithoutLease(
                self.token.app_id.clone(),
            ));
        }
        if self.state != AppInstanceState::Foreground && self.token.lease_id.is_some() {
            return Err(StateModelError::LeaseOutsideForeground(
                self.token.app_id.clone(),
            ));
        }
        Ok(())
    }
}

/// The v2 single source of truth. `domain` owns system/display-domain progress;
/// each application has an independent state record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SupervisorState {
    pub domain: SystemDomainState,
    #[serde(default)]
    pub sleeping: bool,
    #[serde(default)]
    pub foreground_app: Option<AppId>,
    #[serde(default)]
    pub last_app: Option<AppId>,
    #[serde(default)]
    pub apps: BTreeMap<AppId, AppInstance>,
    pub state_revision: u64,
}

impl Default for SupervisorState {
    fn default() -> Self {
        Self {
            domain: SystemDomainState::Stock,
            sleeping: false,
            foreground_app: None,
            last_app: None,
            apps: BTreeMap::new(),
            state_revision: 0,
        }
    }
}

impl SupervisorState {
    pub fn validate(&self) -> Result<(), StateModelError> {
        if self.sleeping && self.domain != SystemDomainState::Managed {
            return Err(StateModelError::SleepOutsideManaged);
        }
        let mut foreground = None;
        let mut leases = BTreeSet::new();
        for (id, instance) in &self.apps {
            if id != &instance.token.app_id {
                return Err(StateModelError::MismatchedAppKey {
                    key: id.clone(),
                    token: instance.token.app_id.clone(),
                });
            }
            instance.validate()?;
            if instance.state == AppInstanceState::Foreground
                && foreground.replace(id.clone()).is_some()
            {
                return Err(StateModelError::MultipleForegroundApps);
            }
            if let Some(lease) = instance.token.lease_id {
                if !leases.insert(lease) {
                    return Err(StateModelError::DuplicateLease(lease));
                }
            }
        }
        if foreground != self.foreground_app {
            return Err(StateModelError::ForegroundPointerMismatch);
        }
        if self.sleeping && self.foreground_app.is_some() {
            return Err(StateModelError::ForegroundWhileSleeping);
        }
        if self.domain != SystemDomainState::Managed {
            if self.foreground_app.is_some() {
                return Err(StateModelError::ForegroundOutsideManaged);
            }
            if self.domain == SystemDomainState::Stock
                && self.apps.values().any(|app| !app.state.is_terminal())
            {
                return Err(StateModelError::RunningAppInStockDomain);
            }
        }
        Ok(())
    }
}

impl SupervisorState {
    pub(super) fn current_instance_mut(
        &mut self,
        app_id: &AppId,
        generation: u64,
    ) -> Result<&mut AppInstance, StateModelError> {
        let instance = self
            .apps
            .get_mut(app_id)
            .ok_or_else(|| StateModelError::UnknownApp(app_id.clone()))?;
        if instance.token.generation != generation {
            return Err(StateModelError::StaleGeneration {
                app_id: app_id.clone(),
                expected_after: instance.token.generation.saturating_sub(1),
                actual: generation,
            });
        }
        Ok(instance)
    }

    pub(super) fn bump_revision(&mut self) -> Result<(), StateModelError> {
        self.state_revision = self
            .state_revision
            .checked_add(1)
            .ok_or(StateModelError::RevisionOverflow)?;
        Ok(())
    }

    pub(super) fn ensure_revision_capacity(&self) -> Result<(), StateModelError> {
        if self.state_revision == u64::MAX {
            Err(StateModelError::RevisionOverflow)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum StateModelError {
    #[error("application {0} has generation zero")]
    ZeroGeneration(AppId),
    #[error("application {0} has lease zero")]
    ZeroLease(AppId),
    #[error("foreground application {0} has no display lease")]
    ForegroundWithoutLease(AppId),
    #[error("non-foreground application {0} holds a display lease")]
    LeaseOutsideForeground(AppId),
    #[error("state map key {key} does not match token app {token}")]
    MismatchedAppKey { key: AppId, token: AppId },
    #[error("multiple applications are marked foreground")]
    MultipleForegroundApps,
    #[error("duplicate display lease {0}")]
    DuplicateLease(u64),
    #[error("foreground_app does not match the foreground instance")]
    ForegroundPointerMismatch,
    #[error("sleep is only valid in the managed domain")]
    SleepOutsideManaged,
    #[error("an application cannot remain foreground while sleeping")]
    ForegroundWhileSleeping,
    #[error("a foreground application exists outside the managed domain")]
    ForegroundOutsideManaged,
    #[error("a non-terminal application exists in the stock domain")]
    RunningAppInStockDomain,
    #[error("invalid system domain transition {from:?} -> {to:?}")]
    InvalidDomainTransition {
        from: SystemDomainState,
        to: SystemDomainState,
    },
    #[error("applications may only launch in the active managed domain")]
    LaunchOutsideActiveManaged,
    #[error("application {0} is already running")]
    AppAlreadyRunning(AppId),
    #[error("unknown application instance {0}")]
    UnknownApp(AppId),
    #[error("stale generation for {app_id}: expected after {expected_after}, got {actual}")]
    StaleGeneration {
        app_id: AppId,
        expected_after: u64,
        actual: u64,
    },
    #[error("stale foreground epoch for {app_id}: expected after {expected_after}, got {actual}")]
    StaleForegroundEpoch {
        app_id: AppId,
        expected_after: u64,
        actual: u64,
    },
    #[error("invalid app transition for {app_id}: {from:?} -> {to:?}")]
    InvalidAppTransition {
        app_id: AppId,
        from: AppInstanceState,
        to: AppInstanceState,
    },
    #[error("state revision overflow")]
    RevisionOverflow,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("transition {transition:?} is invalid while in {current:?}")]
pub struct TransitionError {
    pub current: DomainState,
    pub transition: Transition,
}
