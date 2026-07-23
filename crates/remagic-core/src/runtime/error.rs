use std::path::PathBuf;
use thiserror::Error;

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RuntimeValidationError {
    #[error("invalid platform capability: {0}")]
    InvalidCapability(String),
    #[error("runtime directories are required")]
    MissingDirectories,
    #[error("unsafe {0} path: {1}")]
    UnsafePath(&'static str, PathBuf),
    #[error("invalid required library: {0}")]
    InvalidLibrary(String),
    #[error("duplicate required library: {0}")]
    DuplicateLibrary(String),
    #[error("duplicate {0} path: {1}")]
    DuplicatePath(&'static str, PathBuf),
    #[error("{0} path {1} must be a strict descendant of HOME {2}")]
    DirectoryOutsideHome(&'static str, PathBuf, PathBuf),
    #[error("XDG_RUNTIME_DIR must be ephemeral and outside HOME: {0}")]
    PersistentRuntimeDirectory(PathBuf),
    #[error("invalid {0} value: {1}")]
    InvalidPolicyValue(&'static str, String),
    #[error("a required certificate policy has no CA bundle or directory")]
    MissingCertificateSource,
    #[error("network hosts cannot be declared when network mode is deny or inbound")]
    HostsWithDeniedNetwork,
    #[error("inbound network mode requires a non-privileged listen_port")]
    MissingListenPort,
    #[error("listen_port {0} is only valid for inbound network mode")]
    UnexpectedListenPort(u16),
    #[error("network policy requires OS enforcement, but only policy metadata is available")]
    RequiredNetworkEnforcementUnavailable,
    #[error("invalid allowed network host: {0}")]
    InvalidHost(String),
    #[error("runtime path list cannot be represented in the process environment: {0}")]
    InvalidPathList(String),
    #[error("invalid environment variable: {0}")]
    InvalidEnvironment(String),
    #[error("application environment may not override platform variable {0}")]
    ReservedApplicationEnvironment(String),
    #[error("required library was not resolved: {0}")]
    UnresolvedLibrary(String),
    #[error("launch environment is missing {0}")]
    MissingLaunchVariable(&'static str),
    #[error("launch variable {0} must be {1}, got {2}")]
    MismatchedLaunchVariable(&'static str, PathBuf, String),
    #[error("launch variable {0} disagrees with its policy")]
    PolicyVariableMismatch(&'static str),
    #[error("invalid preflight check id: {0}")]
    InvalidCheckId(String),
    #[error("duplicate preflight check id: {0}")]
    DuplicateCheckId(String),
    #[error("preflight compatible flag disagrees with its results")]
    IncoherentPreflight,
    #[error("a compatible preflight report must contain a launch environment")]
    MissingLaunchEnvironment,
    #[error("preflight app/profile disagrees with its launch environment")]
    PreflightEnvironmentMismatch,
}
