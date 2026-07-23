use super::platform::deduplicate_paths;
use super::{ExecutorError, LaunchDescriptor, PlatformRuntime};
use remagic_core::{
    qtfb_key_for_app, AppManifest, NetworkMode, RuntimeProfile, MANIFEST_SCHEMA_V2,
};
use std::env;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(super) struct ValidatedDescriptor<'a> {
    pub source: &'a LaunchDescriptor,
    pub generation: Option<u64>,
}

pub(super) fn validate_launch_descriptor<'a>(
    manifest: &AppManifest,
    descriptor: &'a LaunchDescriptor,
) -> Result<ValidatedDescriptor<'a>, ExecutorError> {
    let generation = descriptor.generation.filter(|value| *value != 0);
    let foreground_epoch = descriptor.foreground_epoch.filter(|value| *value != 0);
    let lease_id = descriptor.lease_id.filter(|value| *value != 0);
    if manifest.schema == MANIFEST_SCHEMA_V2 {
        if generation.is_none() {
            return Err(ExecutorError::MissingGeneration);
        }
        if foreground_epoch.is_none() {
            return Err(ExecutorError::MissingForegroundEpoch);
        }
        if lease_id.is_none() {
            return Err(ExecutorError::MissingLease);
        }
    }
    Ok(ValidatedDescriptor {
        source: descriptor,
        generation,
    })
}

pub(super) fn validate_profile_contract(manifest: &AppManifest) -> Result<(), ExecutorError> {
    if manifest.schema != MANIFEST_SCHEMA_V2 {
        return Ok(());
    }
    let (display, capability) = match manifest.runtime.profile {
        RuntimeProfile::QtfbCompat => ("qtfb", Some("display:qtfb-v1")),
        RuntimeProfile::NativeV2 => ("quill", Some("display:surface-v2")),
        RuntimeProfile::Headless => ("none", None),
    };
    if manifest.display != display {
        return Err(ExecutorError::ProfileDisplayMismatch {
            profile: manifest.runtime.profile,
            expected: display,
            actual: manifest.display.clone(),
        });
    }
    validate_profile_capability(manifest, capability)?;
    validate_network_capability(manifest)
}

fn validate_profile_capability(
    manifest: &AppManifest,
    required: Option<&'static str>,
) -> Result<(), ExecutorError> {
    let has_capability = |required: &str| {
        manifest
            .capabilities
            .iter()
            .any(|candidate| candidate.as_str() == required)
    };
    match required {
        Some(required) if !has_capability(required) => {
            Err(ExecutorError::ProfileCapabilityMissing {
                profile: manifest.runtime.profile,
                capability: required,
            })
        }
        None if manifest
            .capabilities
            .iter()
            .any(|candidate| candidate.as_str().starts_with("display:")) =>
        {
            Err(ExecutorError::HeadlessDisplayCapability)
        }
        _ => Ok(()),
    }
}

fn validate_network_capability(manifest: &AppManifest) -> Result<(), ExecutorError> {
    let has_outbound = manifest
        .capabilities
        .iter()
        .any(|candidate| candidate.as_str() == "network:outbound-v1");
    match manifest.runtime.network.mode {
        NetworkMode::Deny if has_outbound => Err(ExecutorError::NetworkCapabilityMismatch(
            "deny policy may not request network:outbound-v1",
        )),
        NetworkMode::HttpsOnly | NetworkMode::Outbound if !has_outbound => {
            Err(ExecutorError::NetworkCapabilityMismatch(
                "outbound policy requires network:outbound-v1",
            ))
        }
        _ => Ok(()),
    }
}

pub(super) fn resolve_qtfb_key(
    manifest: &AppManifest,
    descriptor: &LaunchDescriptor,
) -> Result<Option<i32>, ExecutorError> {
    let uses_qtfb = manifest.schema != MANIFEST_SCHEMA_V2
        || manifest.runtime.profile == RuntimeProfile::QtfbCompat;
    if !uses_qtfb {
        return descriptor.qtfb_key.map_or(Ok(None), |_| {
            Err(ExecutorError::UnexpectedQtfbDescriptor(
                manifest.runtime.profile,
            ))
        });
    }
    let expected = qtfb_key_for_app(&manifest.id);
    let actual = descriptor.qtfb_key.unwrap_or(expected);
    if actual <= 0 {
        return Err(ExecutorError::InvalidQtfbKey(actual));
    }
    if actual != expected {
        return Err(ExecutorError::UnexpectedQtfbKey { expected, actual });
    }
    Ok(Some(actual))
}

pub(super) fn validate_platform_capabilities(
    manifest: &AppManifest,
    platform: &PlatformRuntime,
) -> Result<(), ExecutorError> {
    let missing = manifest
        .capabilities
        .iter()
        .filter(|capability| !platform.capabilities.contains(*capability))
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(ExecutorError::MissingCapabilities(missing.join(", ")))
    }
}

pub(super) fn validate_device_compatibility(
    manifest: &AppManifest,
    platform: &PlatformRuntime,
) -> Result<(), ExecutorError> {
    if !manifest.supported_devices.is_empty()
        && !manifest
            .supported_devices
            .contains(&platform.device.product)
    {
        return Err(ExecutorError::UnsupportedDevice(platform.device.product));
    }
    if !manifest.supported_os.is_empty()
        && !manifest
            .supported_os
            .iter()
            .any(|version| version == &platform.device.os_version)
    {
        return Err(ExecutorError::UnsupportedOs(
            platform.device.os_version.clone(),
        ));
    }
    Ok(())
}

pub(super) fn validate_platform_directory_roots(
    manifest: &AppManifest,
    platform: &PlatformRuntime,
) -> Result<(), ExecutorError> {
    if manifest.schema != MANIFEST_SCHEMA_V2 {
        return Ok(());
    }
    let directories = manifest
        .runtime
        .directories
        .as_ref()
        .ok_or_else(|| ExecutorError::Policy("runtime directories are required".into()))?;
    if directories.home != platform.home_root {
        return Err(ExecutorError::UnexpectedHomeRoot {
            expected: platform.home_root.clone(),
            actual: directories.home.clone(),
        });
    }
    let expected = platform.runtime_root.join(manifest.id.as_str());
    if directories.runtime_dir != expected {
        return Err(ExecutorError::UnexpectedRuntimeDirectory {
            expected,
            actual: directories.runtime_dir.clone(),
        });
    }
    Ok(())
}

pub(super) fn validate_program(manifest: &AppManifest) -> Result<(), ExecutorError> {
    let executable = fs::metadata(&manifest.exec)
        .map_err(|source| ExecutorError::Executable(manifest.exec.clone(), source))?;
    if !executable.is_file() || executable.permissions().mode() & 0o111 == 0 {
        return Err(ExecutorError::NotExecutable(manifest.exec.clone()));
    }
    let working_dir = fs::metadata(&manifest.working_dir)
        .map_err(|source| ExecutorError::WorkingDirectory(manifest.working_dir.clone(), source))?;
    if !working_dir.is_dir() {
        return Err(ExecutorError::NotDirectory(manifest.working_dir.clone()));
    }
    Ok(())
}

pub(super) fn validate_runtime_resources(
    manifest: &AppManifest,
    platform: &PlatformRuntime,
) -> Result<(), ExecutorError> {
    if manifest.schema != MANIFEST_SCHEMA_V2 {
        return Ok(());
    }
    if manifest.runtime.profile == RuntimeProfile::QtfbCompat {
        validate_qtfb_socket(&platform.qtfb_socket)?;
    }
    if manifest
        .capabilities
        .iter()
        .any(|capability| capability.as_str() == "agent:pi-v1")
    {
        validate_agent_socket(&platform.agent_socket)?;
    }
    for directory in &manifest.runtime.fonts.directories {
        validate_readable_directory(directory, "font directory")?;
    }
    if let Some(file) = &manifest.runtime.fonts.fontconfig_file {
        validate_readable_regular_file(file, "fontconfig file", true)?;
    }
    if let Some(bundle) = &manifest.runtime.certificates.ca_bundle {
        validate_readable_regular_file(bundle, "CA bundle", true)?;
    }
    if let Some(directory) = &manifest.runtime.certificates.ca_directory {
        validate_readable_directory(directory, "CA directory")?;
    }
    let timezone = platform.zoneinfo_root.join(&manifest.runtime.timezone.name);
    validate_readable_regular_file(&timezone, "timezone data", true)
}

fn validate_qtfb_socket(path: &Path) -> Result<(), ExecutorError> {
    let invalid = !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        });
    if invalid {
        return Err(ExecutorError::InvalidQtfbSocket(path.to_path_buf()));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| ExecutorError::QtfbSocket(path.to_path_buf(), source))?;
    if !metadata.file_type().is_socket() {
        return Err(ExecutorError::InvalidQtfbSocket(path.to_path_buf()));
    }
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid || metadata.permissions().mode() & 0o022 != 0 {
        return Err(ExecutorError::UnsafeQtfbSocket {
            path: path.to_path_buf(),
            expected_uid: effective_uid,
            actual_uid: metadata.uid(),
            mode: metadata.permissions().mode() & 0o777,
        });
    }
    Ok(())
}

fn validate_agent_socket(path: &Path) -> Result<(), ExecutorError> {
    let invalid = !path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        });
    if invalid {
        return Err(ExecutorError::InvalidAgentSocket(path.to_path_buf()));
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| ExecutorError::AgentSocket(path.to_path_buf(), source))?;
    if !metadata.file_type().is_socket() {
        return Err(ExecutorError::InvalidAgentSocket(path.to_path_buf()));
    }
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid || metadata.permissions().mode() & 0o077 != 0 {
        return Err(ExecutorError::UnsafeAgentSocket {
            path: path.to_path_buf(),
            expected_uid: effective_uid,
            actual_uid: metadata.uid(),
            mode: metadata.permissions().mode() & 0o777,
        });
    }
    Ok(())
}

pub(super) fn validate_readable_directory(
    path: &Path,
    kind: &'static str,
) -> Result<(), ExecutorError> {
    let metadata = fs::metadata(path)
        .map_err(|source| ExecutorError::RuntimeResource(kind, path.to_path_buf(), source))?;
    if !metadata.is_dir() {
        return Err(ExecutorError::InvalidRuntimeResource(
            kind,
            path.to_path_buf(),
        ));
    }
    validate_readonly_resource_mode(path, kind, &metadata)?;
    fs::read_dir(path)
        .map_err(|source| ExecutorError::RuntimeResource(kind, path.to_path_buf(), source))?;
    Ok(())
}

fn validate_readable_regular_file(
    path: &Path,
    kind: &'static str,
    require_nonempty: bool,
) -> Result<(), ExecutorError> {
    let metadata = fs::metadata(path)
        .map_err(|source| ExecutorError::RuntimeResource(kind, path.to_path_buf(), source))?;
    if !metadata.is_file() || (require_nonempty && metadata.len() == 0) {
        return Err(ExecutorError::InvalidRuntimeResource(
            kind,
            path.to_path_buf(),
        ));
    }
    validate_readonly_resource_mode(path, kind, &metadata)?;
    if require_nonempty {
        let mut file = fs::File::open(path)
            .map_err(|source| ExecutorError::RuntimeResource(kind, path.to_path_buf(), source))?;
        file.read_exact(&mut [0_u8; 1])
            .map_err(|source| ExecutorError::RuntimeResource(kind, path.to_path_buf(), source))?;
    }
    Ok(())
}

fn validate_readonly_resource_mode(
    path: &Path,
    kind: &'static str,
    metadata: &fs::Metadata,
) -> Result<(), ExecutorError> {
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o022 != 0 {
        return Err(ExecutorError::UnsafeRuntimeResource(
            kind,
            path.to_path_buf(),
            mode,
        ));
    }
    Ok(())
}

pub(super) fn resolve_required_libraries(
    manifest: &AppManifest,
    platform: &PlatformRuntime,
) -> Result<Vec<PathBuf>, ExecutorError> {
    let mut search_dirs = platform.library_search_dirs.clone();
    if manifest.schema != MANIFEST_SCHEMA_V2 {
        if let Some(value) = manifest.environment.get("LD_LIBRARY_PATH") {
            search_dirs.extend(env::split_paths(value));
        }
    }
    deduplicate_paths(&mut search_dirs);
    let mut resolved = Vec::with_capacity(manifest.runtime.required_libraries.len());
    for library in &manifest.runtime.required_libraries {
        let found = search_dirs
            .iter()
            .map(|directory| directory.join(library))
            .find(|candidate| candidate.exists())
            .ok_or_else(|| ExecutorError::MissingLibrary(library.clone()))?;
        validate_readable_regular_file(&found, "dynamic library", true)?;
        resolved.push(found);
    }
    Ok(resolved)
}
