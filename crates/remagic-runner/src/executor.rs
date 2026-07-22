//! Turns a validated manifest and launch descriptor into a process execution
//! plan. Platform discovery, preflight, and environment construction live in
//! separate modules so this facade remains the single orchestration boundary.

use remagic_core::{
    is_platform_reserved_environment, AppManifest, LaunchEnvironment, RuntimeProfile,
    MANIFEST_SCHEMA_V2,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;

mod environment;
mod platform;
mod preflight;

use environment::{
    append_platform_variables, create_runtime_directories, insert_platform_variables,
};
pub(crate) use platform::PlatformRuntime;
#[cfg(test)]
use platform::{DEFAULT_CAPABILITIES, DEFAULT_PATH};
use preflight::{
    resolve_qtfb_key, resolve_required_libraries, validate_launch_descriptor,
    validate_platform_capabilities, validate_platform_directory_roots, validate_profile_contract,
    validate_program, validate_runtime_resources,
};

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(crate) struct LaunchDescriptor {
    #[serde(default)]
    pub open_path: Option<PathBuf>,
    #[serde(default)]
    pub resume_payload: Option<Value>,
    #[serde(default)]
    pub generation: Option<u64>,
    #[serde(default)]
    pub foreground_epoch: Option<u64>,
    #[serde(default)]
    pub lease_id: Option<u64>,
    #[serde(default)]
    pub qtfb_key: Option<i32>,
}

#[derive(Clone, Debug)]
pub(crate) struct ExecutionPlan {
    pub generation: Option<u64>,
    pub variables: BTreeMap<String, String>,
    pub launch_environment: Option<LaunchEnvironment>,
    pub clear_inherited_environment: bool,
}

pub(crate) fn prepare_execution(
    manifest: &AppManifest,
    descriptor: &LaunchDescriptor,
    platform: &PlatformRuntime,
) -> Result<ExecutionPlan, ExecutorError> {
    validate_program(manifest)?;
    manifest
        .runtime
        .validate(manifest.schema == MANIFEST_SCHEMA_V2)
        .map_err(|error| ExecutorError::Policy(error.to_string()))?;
    validate_profile_contract(manifest)?;
    validate_platform_directory_roots(manifest, platform)?;
    validate_platform_capabilities(manifest, platform)?;
    validate_runtime_resources(manifest, platform)?;

    let descriptor = validate_launch_descriptor(manifest, descriptor)?;
    let qtfb_key = resolve_qtfb_key(manifest, descriptor.source)?;
    let resolved_libraries = resolve_required_libraries(manifest, platform)?;
    validate_application_environment(manifest)?;
    preflight::validate_device_compatibility(manifest, platform)?;

    if manifest.schema == MANIFEST_SCHEMA_V2 {
        prepare_v2_execution(manifest, descriptor, qtfb_key, resolved_libraries, platform)
    } else {
        prepare_legacy_execution(manifest, descriptor, qtfb_key, platform)
    }
}

fn validate_application_environment(manifest: &AppManifest) -> Result<(), ExecutorError> {
    if manifest.schema == MANIFEST_SCHEMA_V2 {
        for key in manifest.environment.keys() {
            if is_platform_reserved_environment(key) || key == "QTFB_KEY" {
                return Err(ExecutorError::ReservedEnvironment(key.clone()));
            }
        }
    }
    Ok(())
}

fn prepare_v2_execution(
    manifest: &AppManifest,
    descriptor: preflight::ValidatedDescriptor<'_>,
    qtfb_key: Option<i32>,
    resolved_libraries: Vec<PathBuf>,
    platform: &PlatformRuntime,
) -> Result<ExecutionPlan, ExecutorError> {
    let mut environment = LaunchEnvironment::resolve(
        manifest.id.clone(),
        &manifest.runtime,
        &manifest.environment,
        resolved_libraries.clone(),
        platform.capabilities.clone(),
        platform.path.clone(),
        platform.network_enforcement,
        platform.device.clone(),
    )
    .map_err(|error| ExecutorError::Policy(error.to_string()))?;
    create_runtime_directories(&environment.directories)?;
    append_platform_variables(
        &mut environment.variables,
        &manifest.id,
        descriptor.generation,
        qtfb_key,
        descriptor.source,
        &environment.directories,
        &manifest.runtime.fonts.directories,
        &resolved_libraries,
        platform,
    )?;
    environment
        .validate()
        .map_err(|error| ExecutorError::Policy(error.to_string()))?;
    Ok(ExecutionPlan {
        generation: descriptor.generation,
        variables: environment.variables.clone(),
        launch_environment: Some(environment),
        clear_inherited_environment: true,
    })
}

fn prepare_legacy_execution(
    manifest: &AppManifest,
    descriptor: preflight::ValidatedDescriptor<'_>,
    qtfb_key: Option<i32>,
    platform: &PlatformRuntime,
) -> Result<ExecutionPlan, ExecutorError> {
    let mut variables = manifest.environment.clone();
    insert_platform_variables(
        &mut variables,
        &manifest.id,
        descriptor.generation,
        qtfb_key,
        descriptor.source,
        platform,
    )?;
    Ok(ExecutionPlan {
        generation: descriptor.generation,
        variables,
        launch_environment: None,
        clear_inherited_environment: false,
    })
}

#[derive(Debug, Error)]
pub(crate) enum ExecutorError {
    #[error("application executable {0} is unavailable: {1}")]
    Executable(PathBuf, std::io::Error),
    #[error("application executable is not an executable regular file: {0}")]
    NotExecutable(PathBuf),
    #[error("application working directory {0} is unavailable: {1}")]
    WorkingDirectory(PathBuf, std::io::Error),
    #[error("path is not a directory: {0}")]
    NotDirectory(PathBuf),
    #[error("runtime directory path traverses a symbolic link: {0}")]
    DirectorySymlink(PathBuf),
    #[error("runtime directory {path} must be owned by uid {expected}, got {actual}")]
    DirectoryOwner {
        path: PathBuf,
        expected: u32,
        actual: u32,
    },
    #[error("runtime directory has unsafe mode {1:o}: {0}")]
    UnsafeDirectoryMode(PathBuf, u32),
    #[error("schema v2 HOME must be the platform root {expected}, got {actual}")]
    UnexpectedHomeRoot { expected: PathBuf, actual: PathBuf },
    #[error("schema v2 runtime directory must be {expected}, got {actual}")]
    UnexpectedRuntimeDirectory { expected: PathBuf, actual: PathBuf },
    #[error("could not secure runtime directory {0}: {1}")]
    DirectoryPermissions(PathBuf, std::io::Error),
    #[error("runtime resource {0} is unavailable at {1}: {2}")]
    RuntimeResource(&'static str, PathBuf, std::io::Error),
    #[error("runtime resource {0} has the wrong type or is empty: {1}")]
    InvalidRuntimeResource(&'static str, PathBuf),
    #[error("runtime resource {0} has unsafe writable mode {2:o}: {1}")]
    UnsafeRuntimeResource(&'static str, PathBuf, u32),
    #[error("runtime resource is not readable: {0}: {1}")]
    UnreadableResource(PathBuf, std::io::Error),
    #[error("could not create runtime directory {0}: {1}")]
    CreateDirectory(PathBuf, std::io::Error),
    #[error("schema v2 launch descriptor is missing a non-zero generation")]
    MissingGeneration,
    #[error("schema v2 launch descriptor is missing a non-zero foreground epoch")]
    MissingForegroundEpoch,
    #[error("schema v2 launch descriptor is missing a non-zero display lease")]
    MissingLease,
    #[error("runtime profile {0:?} may not receive a QTFB descriptor")]
    UnexpectedQtfbDescriptor(RuntimeProfile),
    #[error("invalid QTFB key {0}")]
    InvalidQtfbKey(i32),
    #[error("QTFB key mismatch: expected {expected}, got {actual}")]
    UnexpectedQtfbKey { expected: i32, actual: i32 },
    #[error("QTFB socket is unavailable at {0}: {1}")]
    QtfbSocket(PathBuf, std::io::Error),
    #[error("QTFB socket path is invalid or is not a Unix socket: {0}")]
    InvalidQtfbSocket(PathBuf),
    #[error(
        "QTFB socket {path} is not private platform state: expected uid {expected_uid}, got uid {actual_uid}, mode {mode:o}"
    )]
    UnsafeQtfbSocket {
        path: PathBuf,
        expected_uid: u32,
        actual_uid: u32,
        mode: u32,
    },
    #[error("runtime profile {profile:?} requires display={expected}, got {actual}")]
    ProfileDisplayMismatch {
        profile: RuntimeProfile,
        expected: &'static str,
        actual: String,
    },
    #[error("runtime profile {profile:?} did not declare capability {capability}")]
    ProfileCapabilityMissing {
        profile: RuntimeProfile,
        capability: &'static str,
    },
    #[error("headless runtime may not request display capabilities")]
    HeadlessDisplayCapability,
    #[error("network policy/capability mismatch: {0}")]
    NetworkCapabilityMismatch(&'static str),
    #[error("platform is missing required capabilities: {0}")]
    MissingCapabilities(String),
    #[error("application does not support device {0:?}")]
    UnsupportedDevice(remagic_core::DeviceProduct),
    #[error("application does not support ReMagic OS build {0}")]
    UnsupportedOs(String),
    #[error("required library is unavailable: {0}")]
    MissingLibrary(String),
    #[error("schema v2 application attempted to override reserved variable {0}")]
    ReservedEnvironment(String),
    #[error("runtime policy is invalid: {0}")]
    Policy(String),
    #[error("path is not valid UTF-8: {0}")]
    NonUtf8Path(PathBuf),
    #[error("could not join runtime paths: {0}")]
    JoinPaths(String),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests;
