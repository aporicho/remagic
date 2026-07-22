//! Canonical manager control-plane wire types and the explicit v1 conversion
//! boundary. Message families stay together so schema review remains atomic.

use crate::{Envelope, PackageOperation};
use remagic_core::{
    AppId, AppInstance, AppKind, AppSession, Capability, DeviceProduct, PreflightReport,
    RuntimeProfile, SupervisorState, SystemDomainState, UninstallPolicy,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

pub type ControlRequest = Envelope<ControlIntent>;
pub type ControlResponse = Envelope<ControlReply>;
pub type ControlEventEnvelope = Envelope<ControlEvent>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "intent", rename_all = "snake_case")]
pub enum ControlIntent {
    Snapshot,
    Subscribe {
        #[serde(default)]
        since_revision: Option<u64>,
    },
    ReloadManifests,
    ShowHome,
    ReturnStock,
    Sleep,
    Wake,
    Launch {
        app_id: AppId,
        #[serde(default)]
        open_path: Option<PathBuf>,
    },
    OpenPath {
        app_id: AppId,
        path: PathBuf,
    },
    ParkCurrent,
    Close {
        app_id: AppId,
    },
    Preflight {
        app_id: AppId,
    },
    Install {
        bundle: PathBuf,
    },
    Upgrade {
        app_id: AppId,
        #[serde(default)]
        bundle: Option<PathBuf>,
    },
    Rollback {
        app_id: AppId,
        #[serde(default)]
        version: Option<String>,
    },
    Uninstall {
        app_id: AppId,
        #[serde(default)]
        purge: bool,
    },
    /// Temporary bridge for v1 package-provider operations.
    LegacyPackage {
        operation: PackageOperation,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ControlReply {
    Ack {
        state_revision: u64,
    },
    Snapshot {
        snapshot: SupervisorSnapshot,
    },
    Subscribed {
        state_revision: u64,
    },
    Preflight {
        report: Box<PreflightReport>,
    },
    PackageOutput {
        success: bool,
        output: String,
        state_revision: u64,
    },
    Error {
        code: ControlErrorCode,
        message: String,
        #[serde(default)]
        state_revision: Option<u64>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlErrorCode {
    InvalidRequest,
    RevisionConflict,
    AppNotFound,
    AppBusy,
    PermissionDenied,
    PreflightFailed,
    UnsupportedDevice,
    UnsupportedOs,
    PackageInvalid,
    SignatureInvalid,
    Timeout,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ControlEvent {
    SnapshotChanged {
        snapshot: SupervisorSnapshot,
    },
    DomainChanged {
        domain: SystemDomainState,
        sleeping: bool,
        state_revision: u64,
    },
    AppChanged {
        app_id: AppId,
        #[serde(default)]
        instance: Option<AppInstance>,
        state_revision: u64,
    },
    Notification {
        app_id: AppId,
        title: String,
        body: String,
        state_revision: u64,
    },
    PackageProgress {
        app_id: AppId,
        phase: String,
        completed: u64,
        total: u64,
        state_revision: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SupervisorSnapshot {
    pub state: SupervisorState,
    #[serde(default)]
    pub apps: Vec<AppViewV2>,
}

impl SupervisorSnapshot {
    pub fn validate(&self) -> Result<(), remagic_core::StateModelError> {
        self.state.validate()
    }

    pub fn to_v1_status(&self) -> crate::Response {
        let state = remagic_core::ManagerState::from(&self.state);
        crate::Response::Status {
            domain: state.domain,
            last_app: state.last_app,
            sequence: state.sequence,
        }
    }

    pub fn to_v1_apps(&self) -> crate::Response {
        crate::Response::Apps {
            apps: self.apps.iter().map(AppViewV2::to_v1).collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppViewV2 {
    pub id: AppId,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub kind: AppKind,
    pub installed: bool,
    pub runtime_profile: RuntimeProfile,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub instance: Option<AppInstance>,
    #[serde(default)]
    pub background_service: Option<String>,
    #[serde(default)]
    pub background_active: bool,
    #[serde(default)]
    pub session: Option<AppSession>,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub supported_devices: Vec<DeviceProduct>,
    #[serde(default)]
    pub supported_os: Vec<String>,
    #[serde(default)]
    pub required_remagic_api: u32,
    #[serde(default)]
    pub uninstall_policy: UninstallPolicy,
    #[serde(default)]
    pub preflight: Option<PreflightReport>,
}

impl AppViewV2 {
    pub fn to_v1(&self) -> crate::AppView {
        crate::AppView {
            id: self.id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            installed: self.installed,
            foreground: self.instance.as_ref().is_some_and(|instance| {
                instance.state == remagic_core::AppInstanceState::Foreground
            }),
            background_service: self.background_service.clone(),
            background_active: self.background_active,
            session: self.session.clone(),
            package: self.package.clone(),
        }
    }
}

impl TryFrom<crate::Request> for ControlIntent {
    type Error = LegacyControlConversionError;

    fn try_from(request: crate::Request) -> Result<Self, Self::Error> {
        use crate::Request;
        Ok(match request {
            Request::Status | Request::ListApps => Self::Snapshot,
            Request::ReloadManifests => Self::ReloadManifests,
            Request::OpenManager => Self::ShowHome,
            Request::ReturnSystem => Self::ReturnStock,
            Request::Sleep { .. } => Self::Sleep,
            Request::Wake { .. } => Self::Wake,
            Request::Launch { app_id, open_path } => Self::Launch { app_id, open_path },
            Request::ParkCurrent => Self::ParkCurrent,
            Request::Close { app_id, .. } => Self::Close { app_id },
            Request::Package { operation } => Self::LegacyPackage { operation },
            Request::RuntimeExited { .. }
            | Request::Ready { .. }
            | Request::Parked { .. }
            | Request::Notify { .. } => return Err(LegacyControlConversionError::LifecycleRequest),
        })
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum LegacyControlConversionError {
    #[error("v1 application callback belongs on the lifecycle channel")]
    LifecycleRequest,
}

#[cfg(test)]
mod tests;
