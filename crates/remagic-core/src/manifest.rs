use crate::runtime::{
    Capability, NetworkMode, RuntimeProfile, RuntimeRequirements, RuntimeValidationError,
};
use remagic_device::DeviceProduct;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use thiserror::Error;

mod store;
mod validation;

pub use store::ManifestStore;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AppId(String);

impl AppId {
    pub fn new(value: impl Into<String>) -> Result<Self, ManifestError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && !value.starts_with('-')
            && !value.ends_with('-');
        if valid {
            Ok(Self(value))
        } else {
            Err(ManifestError::InvalidId(value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for AppId {
    type Error = ManifestError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<AppId> for String {
    fn from(value: AppId) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParkStrategy {
    /// Persist UI state and exit. Recall starts a fresh UI process.
    #[default]
    Restart,
    /// Keep a process alive after it has acknowledged display/input release.
    Resident,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    #[default]
    User,
    System,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UninstallPolicy {
    #[default]
    KeepData,
    Purge,
}

pub const MANIFEST_SCHEMA_V1: u32 = 1;
pub const MANIFEST_SCHEMA_V2: u32 = 2;
pub const REMAGIC_APP_API_VERSION: u32 = 2;
pub const MAX_STARTUP_TIMEOUT_MS: u64 = 1_450_000;

fn default_required_remagic_api() -> u32 {
    1
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessMode {
    /// The process remaining alive is sufficient. Primarily a v1 fallback.
    #[default]
    Process,
    /// Wait for an explicit lifecycle `ready` event.
    Lifecycle,
    /// Wait for lifecycle readiness and the first stable surface commit.
    FirstFrame,
    /// Legacy file-based readiness, retained only for compatibility adapters.
    File,
}

fn default_readiness_timeout_ms() -> u64 {
    15_000
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadinessPolicy {
    #[serde(default)]
    pub mode: ReadinessMode,
    #[serde(default = "default_readiness_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub path: Option<PathBuf>,
}

impl Default for ReadinessPolicy {
    fn default() -> Self {
        Self {
            mode: ReadinessMode::Process,
            timeout_ms: default_readiness_timeout_ms(),
            path: None,
        }
    }
}

fn default_graceful_timeout_ms() -> u64 {
    3_500
}

fn default_term_timeout_ms() -> u64 {
    4_500
}

fn default_kill_timeout_ms() -> u64 {
    5_500
}

/// Maximum absolute application shutdown deadline supported by the platform
/// service fence. The remaining 2.5 seconds cover runner drain, the bounded
/// exit callback, and scheduling before `TimeoutStopSec=8` is the backstop.
pub const MAX_SHUTDOWN_KILL_TIMEOUT_MS: u64 = 5_500;

/// Absolute deadlines from the beginning of a shutdown request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ShutdownPolicy {
    #[serde(default = "default_graceful_timeout_ms")]
    pub graceful_timeout_ms: u64,
    #[serde(default = "default_term_timeout_ms")]
    pub term_timeout_ms: u64,
    #[serde(default = "default_kill_timeout_ms")]
    pub kill_timeout_ms: u64,
}

impl Default for ShutdownPolicy {
    fn default() -> Self {
        Self {
            graceful_timeout_ms: default_graceful_timeout_ms(),
            term_timeout_ms: default_term_timeout_ms(),
            kill_timeout_ms: default_kill_timeout_ms(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundRestartPolicy {
    #[default]
    Never,
    OnFailure,
    Always,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BackgroundService {
    /// A platform-supervised headless child process.
    Managed {
        exec: PathBuf,
        #[serde(default)]
        args: Vec<String>,
        working_dir: PathBuf,
        #[serde(default)]
        restart: BackgroundRestartPolicy,
    },
    /// Transitional support for an already-installed systemd unit.
    Systemd { unit: String },
}

fn default_migration_timeout_ms() -> u64 {
    120_000
}

fn default_backup_timeout_ms() -> u64 {
    120_000
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DataSchema {
    pub version: u32,
    #[serde(default)]
    pub migrator: Option<PathBuf>,
    #[serde(default)]
    pub backup_paths: Vec<PathBuf>,
    /// Cold-start allowance for pending recovery and the durable pre-change
    /// snapshot. This is separate from the migrator's own execution budget.
    #[serde(default = "default_backup_timeout_ms")]
    pub backup_timeout_ms: u64,
    #[serde(default = "default_migration_timeout_ms")]
    pub migration_timeout_ms: u64,
}

fn default_schema() -> u32 {
    1
}

fn default_display() -> String {
    "quill".into()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppManifest {
    #[serde(default = "default_schema")]
    pub schema: u32,
    pub id: AppId,
    pub name: String,
    #[serde(default)]
    pub kind: AppKind,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub icon: Option<PathBuf>,
    #[serde(default)]
    pub package: Option<String>,
    /// Empty means compatibility is inherited from the installed ReMagic
    /// platform. Store-delivered packages declare every supported product.
    #[serde(default)]
    pub supported_devices: Vec<DeviceProduct>,
    /// Exact OS build identifiers. Empty inherits the system package's
    /// stricter OS compatibility gate.
    #[serde(default)]
    pub supported_os: Vec<String>,
    #[serde(default = "default_required_remagic_api")]
    pub required_remagic_api: u32,
    #[serde(default)]
    pub uninstall_policy: UninstallPolicy,
    pub exec: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub working_dir: PathBuf,
    #[serde(default = "default_display")]
    pub display: String,
    #[serde(default)]
    pub park_strategy: ParkStrategy,
    /// Schema v2 lifecycle policy. In schema v1 `park_strategy` remains the
    /// source of truth.
    #[serde(default)]
    pub resident: bool,
    /// Platform capabilities required before this application may launch.
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub readiness: ReadinessPolicy,
    #[serde(default)]
    pub shutdown: ShutdownPolicy,
    #[serde(default)]
    pub background_service: Option<BackgroundService>,
    #[serde(default)]
    pub data_schema: Option<DataSchema>,
    #[serde(default)]
    pub runtime: RuntimeRequirements,
    /// Legacy schema v1 systemd background unit.
    #[serde(default)]
    pub background_unit: Option<String>,
    #[serde(default)]
    pub supports_open_path: bool,
    #[serde(default)]
    pub allowed_open_roots: Vec<PathBuf>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
}

impl AppManifest {
    /// Maximum cold-launch interval before the application can publish its
    /// readiness signal. Backup/recovery, migration, and readiness are
    /// sequential phases with independent allowances.
    pub fn startup_timeout_ms(&self) -> u64 {
        let (backup, migration, commit) = self.data_schema.as_ref().map_or((0, 0, 0), |schema| {
            (
                schema.backup_timeout_ms,
                schema.migration_timeout_ms,
                crate::SCHEMA_COMMIT_GRACE_MS,
            )
        });
        backup
            .saturating_add(migration)
            .saturating_add(commit)
            .saturating_add(self.readiness.timeout_ms)
            .saturating_add(if self.display == "qtfb" {
                self.readiness.timeout_ms
            } else {
                0
            })
            .min(MAX_STARTUP_TIMEOUT_MS)
    }
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("invalid application id: {0}")]
    InvalidId(String),
    #[error("unsupported manifest schema {0}")]
    UnsupportedSchema(u32),
    #[error("application {0} has an empty name")]
    EmptyName(String),
    #[error("unsafe {0} path: {1}")]
    UnsafePath(&'static str, PathBuf),
    #[error("unsupported display backend: {0}")]
    UnsupportedDisplay(String),
    #[error("unsafe environment key: {0}")]
    UnsafeEnvironment(String),
    #[error("application argument contains NUL")]
    InvalidArgument,
    #[error("duplicate platform capability: {0}")]
    DuplicateCapability(String),
    #[error("duplicate supported device: {0:?}")]
    DuplicateSupportedDevice(DeviceProduct),
    #[error("duplicate or empty supported OS build: {0:?}")]
    InvalidSupportedOs(String),
    #[error("application requires unsupported ReMagic API {0}")]
    UnsupportedRemagicApi(u32),
    #[error("readiness timeout must be between 100 and 120000 ms, got {0}")]
    InvalidReadinessTimeout(u64),
    #[error("file readiness requires an absolute readiness.path")]
    MissingReadinessPath,
    #[error("readiness.path is only valid for file readiness")]
    UnexpectedReadinessPath,
    #[error(
        "invalid shutdown deadlines: graceful={graceful_timeout_ms}, term={term_timeout_ms}, kill={kill_timeout_ms}"
    )]
    InvalidShutdownPolicy {
        graceful_timeout_ms: u64,
        term_timeout_ms: u64,
        kill_timeout_ms: u64,
    },
    #[error("invalid background systemd unit: {0}")]
    InvalidSystemdUnit(String),
    #[error("schema v2 resident conflicts with legacy park_strategy")]
    ConflictingResidentPolicy,
    #[error("runtime.background_execution=freeze requires manifest schema v2")]
    FreezeRequiresV2,
    #[error("runtime.background_execution=freeze requires resident=true")]
    FreezeRequiresResident,
    #[error("runtime.background_execution=freeze requires capability lifecycle:v2")]
    FreezeRequiresLifecycleV2,
    #[error("legacy field {0} is not valid in a schema v2 manifest")]
    LegacyFieldInV2(&'static str),
    #[error(
        "runtime profile {profile:?} requires display={expected}, but manifest declares {actual}"
    )]
    RuntimeDisplayMismatch {
        profile: RuntimeProfile,
        expected: &'static str,
        actual: String,
    },
    #[error("runtime profile {profile:?} requires capability {capability}")]
    MissingRuntimeCapability {
        profile: RuntimeProfile,
        capability: &'static str,
    },
    #[error("headless runtime may not request a display capability")]
    HeadlessDisplayCapability,
    #[error("network mode {mode:?} requires {expected}")]
    NetworkCapabilityMismatch {
        mode: NetworkMode,
        expected: &'static str,
    },
    #[error("data schema version must be greater than zero")]
    InvalidDataSchemaVersion,
    #[error("data_schema requires manifest schema v2")]
    DataSchemaRequiresV2,
    #[error("backup/recovery timeout must be between 100 and 600000 ms, got {0}")]
    InvalidBackupTimeout(u64),
    #[error("migration timeout must be between 100 and 600000 ms, got {0}")]
    InvalidMigrationTimeout(u64),
    #[error("duplicate data backup path: {0}")]
    DuplicateBackupPath(PathBuf),
    #[error("data backup paths overlap: {0} and {1}")]
    OverlappingBackupPaths(PathBuf, PathBuf),
    #[error("invalid runtime policy: {0}")]
    Runtime(#[from] RuntimeValidationError),
    #[error("application {0} does not support open-path")]
    OpenPathUnsupported(String),
    #[error("open path is outside the application's allowed roots: {0}")]
    OpenPathDenied(PathBuf),
    #[error("cannot canonicalize {0}: {1}")]
    Canonicalize(PathBuf, #[source] std::io::Error),
    #[error("cannot list manifest directory {0}: {1}")]
    ReadDir(PathBuf, #[source] std::io::Error),
    #[error("cannot read manifest {0}: {1}")]
    Read(PathBuf, #[source] std::io::Error),
    #[error("cannot parse manifest {0}: {1}")]
    Parse(PathBuf, #[source] toml::de::Error),
    #[error("duplicate application id in {0}")]
    DuplicateId(PathBuf),
}

#[cfg(test)]
mod tests;
