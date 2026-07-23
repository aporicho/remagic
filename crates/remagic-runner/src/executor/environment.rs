use super::platform::{deduplicate_paths, APPROVED_LIBRARY_DIRS};
use super::preflight::validate_readable_directory;
use super::{ExecutorError, LaunchDescriptor, PlatformRuntime};
use remagic_core::{AppId, Capability, RuntimeDirectories};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub(super) fn create_runtime_directories(
    directories: &RuntimeDirectories,
) -> Result<(), ExecutorError> {
    directories
        .validate()
        .map_err(|error| ExecutorError::Policy(error.to_string()))?;
    for directory in configured_directories(directories) {
        create_directory_without_symlinks(directory)?;
        validate_owned_directory(directory)?;
    }
    for directory in private_directories(directories) {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
            .map_err(|source| ExecutorError::DirectoryPermissions(directory.clone(), source))?;
        let mode = fs::metadata(directory)
            .map_err(|source| ExecutorError::CreateDirectory(directory.clone(), source))?
            .permissions()
            .mode()
            & 0o777;
        if mode != 0o700 {
            return Err(ExecutorError::UnsafeDirectoryMode(directory.clone(), mode));
        }
    }
    Ok(())
}

fn configured_directories(directories: &RuntimeDirectories) -> [&PathBuf; 6] {
    [
        &directories.home,
        &directories.config_home,
        &directories.data_home,
        &directories.state_home,
        &directories.cache_home,
        &directories.runtime_dir,
    ]
}

fn private_directories(directories: &RuntimeDirectories) -> [&PathBuf; 5] {
    [
        &directories.config_home,
        &directories.data_home,
        &directories.state_home,
        &directories.cache_home,
        &directories.runtime_dir,
    ]
}

fn create_directory_without_symlinks(path: &Path) -> Result<(), ExecutorError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ExecutorError::DirectorySymlink(current));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(ExecutorError::NotDirectory(current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut builder = fs::DirBuilder::new();
                builder.mode(0o700);
                builder
                    .create(&current)
                    .map_err(|source| ExecutorError::CreateDirectory(current.clone(), source))?;
            }
            Err(source) => {
                return Err(ExecutorError::CreateDirectory(current.clone(), source));
            }
        }
    }
    Ok(())
}

fn validate_owned_directory(path: &Path) -> Result<(), ExecutorError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|source| ExecutorError::CreateDirectory(path.to_path_buf(), source))?;
    if metadata.file_type().is_symlink() {
        return Err(ExecutorError::DirectorySymlink(path.to_path_buf()));
    }
    if !metadata.is_dir() {
        return Err(ExecutorError::NotDirectory(path.to_path_buf()));
    }
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(ExecutorError::DirectoryOwner {
            path: path.to_path_buf(),
            expected: effective_uid,
            actual: metadata.uid(),
        });
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o022 != 0 {
        return Err(ExecutorError::UnsafeDirectoryMode(path.to_path_buf(), mode));
    }
    fs::read_dir(path)
        .map_err(|source| ExecutorError::UnreadableResource(path.to_path_buf(), source))?;
    Ok(())
}

pub(super) fn insert_platform_variables(
    variables: &mut BTreeMap<String, String>,
    app_id: &AppId,
    generation: Option<u64>,
    qtfb_key: Option<i32>,
    descriptor: &LaunchDescriptor,
    platform: &PlatformRuntime,
) -> Result<(), ExecutorError> {
    variables.insert("PATH".into(), platform.path.clone());
    if let Some(qtfb_key) = qtfb_key {
        variables.insert("QTFB_KEY".into(), qtfb_key.to_string());
        variables.insert(
            "REMAGIC_QTFB_SOCKET".into(),
            path_text(&platform.qtfb_socket)?,
        );
    }
    variables.insert(
        "REMAGIC_SOCKET".into(),
        remagic_protocol::DEFAULT_SOCKET.into(),
    );
    variables.insert("REMAGIC_APP_ID".into(), app_id.to_string());
    variables.insert(
        "REMAGIC_LAUNCH_ID".into(),
        format!("{}-{}", app_id, std::process::id()),
    );
    variables.insert("REMAGIC_MANAGED".into(), "1".into());
    insert_token_variables(variables, app_id, generation, descriptor)?;
    insert_descriptor_variables(variables, descriptor)?;
    Ok(())
}

pub(super) fn insert_agent_variable(
    variables: &mut BTreeMap<String, String>,
    platform: &PlatformRuntime,
) -> Result<(), ExecutorError> {
    variables.insert(
        "REMAGIC_AGENT_SOCKET".into(),
        path_text(&platform.agent_socket)?,
    );
    variables.insert("REMAGIC_AGENT_TOKEN".into(), random_agent_token()?);
    variables.insert("REMAGIC_AGENT_PRINCIPAL".into(), "foreground".into());
    Ok(())
}

fn random_agent_token() -> Result<String, ExecutorError> {
    let mut bytes = [0_u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(ExecutorError::AgentToken)?;
    let mut token = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut token, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(token)
}

fn insert_token_variables(
    variables: &mut BTreeMap<String, String>,
    app_id: &AppId,
    generation: Option<u64>,
    descriptor: &LaunchDescriptor,
) -> Result<(), ExecutorError> {
    let Some(generation) = generation else {
        return Ok(());
    };
    variables.insert("REMAGIC_APP_GENERATION".into(), generation.to_string());
    let token = serde_json::json!({
        "app_id": app_id,
        "generation": generation,
        "foreground_epoch": descriptor.foreground_epoch.unwrap_or(0),
        "lease_id": descriptor.lease_id,
    });
    variables.insert("REMAGIC_APP_TOKEN".into(), serde_json::to_string(&token)?);
    Ok(())
}

fn insert_descriptor_variables(
    variables: &mut BTreeMap<String, String>,
    descriptor: &LaunchDescriptor,
) -> Result<(), ExecutorError> {
    if let Some(epoch) = descriptor.foreground_epoch {
        variables.insert("REMAGIC_FOREGROUND_EPOCH".into(), epoch.to_string());
    }
    if let Some(lease_id) = descriptor.lease_id {
        variables.insert("REMAGIC_DISPLAY_LEASE_ID".into(), lease_id.to_string());
    }
    if let Some(payload) = descriptor
        .resume_payload
        .as_ref()
        .filter(|value| !value.is_null())
    {
        variables.insert(
            "REMAGIC_RESUME_PAYLOAD".into(),
            serde_json::to_string(payload)?,
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn append_platform_variables(
    variables: &mut BTreeMap<String, String>,
    app_id: &AppId,
    generation: Option<u64>,
    qtfb_key: Option<i32>,
    descriptor: &LaunchDescriptor,
    directories: &RuntimeDirectories,
    font_directories: &[PathBuf],
    resolved_libraries: &[PathBuf],
    capabilities: &[Capability],
    platform: &PlatformRuntime,
) -> Result<(), ExecutorError> {
    insert_platform_variables(
        variables, app_id, generation, qtfb_key, descriptor, platform,
    )?;
    variables.insert(
        "REMAGIC_RUNTIME_DIR".into(),
        path_text(&directories.runtime_dir)?,
    );
    insert_font_directories(variables, font_directories)?;
    variables.insert(
        "LD_LIBRARY_PATH".into(),
        approved_library_path(resolved_libraries, platform)?,
    );
    insert_shared_storage_variables(variables, capabilities, platform)?;
    Ok(())
}

fn insert_shared_storage_variables(
    variables: &mut BTreeMap<String, String>,
    capabilities: &[Capability],
    platform: &PlatformRuntime,
) -> Result<(), ExecutorError> {
    for (capability, variable, path) in [
        (
            "storage:books-write-v1",
            "REMAGIC_BOOKS_DIR",
            platform.home_root.join("books"),
        ),
        (
            "storage:wallpapers-write-v1",
            "REMAGIC_WALLPAPERS_DIR",
            platform.home_root.join(".local/share/remagic/wallpapers"),
        ),
    ] {
        if !capabilities
            .iter()
            .any(|candidate| candidate.as_str() == capability)
        {
            continue;
        }
        create_directory_without_symlinks(&path)?;
        validate_owned_directory(&path)?;
        variables.insert(variable.into(), path_text(&path)?);
    }
    Ok(())
}

fn insert_font_directories(
    variables: &mut BTreeMap<String, String>,
    font_directories: &[PathBuf],
) -> Result<(), ExecutorError> {
    if !font_directories.is_empty() {
        let directories = join_paths(font_directories)?;
        variables.insert("QT_QPA_FONTDIR".into(), directories.clone());
        variables.insert("REMAGIC_FONT_DIRECTORIES".into(), directories);
    }
    Ok(())
}

fn approved_library_path(
    resolved_libraries: &[PathBuf],
    platform: &PlatformRuntime,
) -> Result<String, ExecutorError> {
    let mut directories = APPROVED_LIBRARY_DIRS
        .iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    directories.extend(platform.library_search_dirs.clone());
    directories.extend(
        resolved_libraries
            .iter()
            .filter_map(|library| library.parent().map(Path::to_path_buf)),
    );
    deduplicate_paths(&mut directories);
    directories.retain(|directory| directory.is_dir());
    for directory in &directories {
        validate_readable_directory(directory, "dynamic library directory")?;
    }
    join_paths(&directories)
}

fn path_text(path: &Path) -> Result<String, ExecutorError> {
    path.to_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| ExecutorError::NonUtf8Path(path.to_path_buf()))
}

fn join_paths(paths: &[PathBuf]) -> Result<String, ExecutorError> {
    env::join_paths(paths)
        .map_err(|error| ExecutorError::JoinPaths(error.to_string()))?
        .into_string()
        .map_err(|_| ExecutorError::JoinPaths("joined path is not UTF-8".into()))
}
