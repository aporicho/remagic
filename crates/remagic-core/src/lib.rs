//! Shared model and persistence for ReMagic.

pub mod manifest;
pub mod power;
pub mod runtime;
pub mod session;
pub mod state;

pub use remagic_device::{
    DeviceDisplayProfile, DeviceProduct, DeviceProfile, DeviceProfileError, SurfacePixelFormat,
    DEVICE_PROFILE_ENV, DEVICE_PROFILE_SCHEMA_V1,
};

pub use manifest::{
    AppId, AppKind, AppManifest, BackgroundRestartPolicy, BackgroundService, DataSchema,
    ManifestStore, ParkStrategy, ReadinessMode, ReadinessPolicy, ShutdownPolicy, SyncProvider,
    UninstallPolicy, MANIFEST_SCHEMA_V1, MANIFEST_SCHEMA_V2, MAX_SHUTDOWN_KILL_TIMEOUT_MS,
    REMAGIC_APP_API_VERSION,
};
pub use runtime::{
    is_platform_reserved_environment, qtfb_key_for_app, validate_environment_pair,
    BackgroundExecution, Capability, CertificatePolicy, FontPolicy, LaunchEnvironment,
    LocalePolicy, NetworkMode, NetworkPolicy, PreflightCheck, PreflightReport, PreflightStatus,
    RuntimeDirectories, RuntimeProfile, RuntimeRequirements, RuntimeValidationError,
    TimezonePolicy, REMAGIC_HOME_QTFB_KEY,
};
pub use session::{AppSession, SessionStatus, SessionStore};
pub use state::{
    AppInstance, AppInstanceState, AppToken, DomainState, ManagerState, StateModelError,
    SupervisorState, SystemDomainState, Transition, TransitionError,
};

pub const SYSTEM_APP_ID: &str = "system";
pub const HOME_APP_ID: &str = "remagic-home";
pub const SCHEMA_PREPARED_FILE: &str = "schema-prepared";
pub const SCHEMA_COMPLETE_FILE: &str = "schema-complete";
pub const SCHEMA_READY_FILE: &str = "schema-ready";
pub const SCHEMA_COMMIT_GRACE_MS: u64 = 10_000;
