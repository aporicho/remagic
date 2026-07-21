use crate::data_schema;
use crate::executor::{prepare_execution, ExecutionPlan, LaunchDescriptor, PlatformRuntime};
use crate::lifecycle_bridge::{ControlSocket, LifecycleBridge, LifecycleStatusStore};
use remagic_core::{AppId, AppManifest, ManifestStore, MANIFEST_SCHEMA_V2};
use serde_json::Value;
use std::fs;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

const MANIFEST_ROOT: &str = "/home/root/.local/share/remagic/apps.d";
const LAUNCH_ROOT: &str = "/run/remagic/launch";
const MAX_LAUNCH_DESCRIPTOR: u64 = 1024 * 1024;

pub(crate) struct LifecycleResources {
    pub bridge: Option<LifecycleBridge>,
    pub child_descriptor: Option<OwnedFd>,
    pub control_socket: Option<ControlSocket>,
    pub status_store: Option<LifecycleStatusStore>,
}

pub(crate) struct PreparedApplication {
    pub id: AppId,
    pub manifest: AppManifest,
    pub open_path: Option<PathBuf>,
    pub resume_payload: Option<Value>,
    pub plan: ExecutionPlan,
    pub lifecycle: LifecycleResources,
}

pub(crate) fn prepare() -> Result<PreparedApplication, Box<dyn std::error::Error>> {
    let (id, manifest) = load_manifest()?;
    fs::create_dir_all("/run/remagic")?;
    let launch = consume_launch_descriptor(&id, manifest.schema)?;
    let open_path = launch
        .open_path
        .as_deref()
        .map(|path| manifest.validate_open_path(path))
        .transpose()?;
    let resume_payload = launch
        .resume_payload
        .clone()
        .filter(|value| !value.is_null());
    let platform = PlatformRuntime::from_process()?;
    let mut plan = prepare_execution(&manifest, &launch, &platform)?;
    // Schema migration is deliberately completed before lifecycle sockets,
    // control endpoints, or the application process can become observable.
    let schema_ready = data_schema::apply(&manifest, &plan)?;
    let lifecycle = prepare_lifecycle(&id, &manifest, &launch, &schema_ready, &mut plan)?;
    Ok(PreparedApplication {
        id,
        manifest,
        open_path,
        resume_payload,
        plan,
        lifecycle,
    })
}

fn load_manifest() -> Result<(AppId, AppManifest), Box<dyn std::error::Error>> {
    let raw_id = std::env::args()
        .nth(1)
        .ok_or("usage: remagic-runner <app-id>")?;
    let id = AppId::new(raw_id)?;
    let root = std::env::var_os("REMAGIC_MANIFEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| MANIFEST_ROOT.into());
    let manifest = ManifestStore::new(root)
        .load_all()?
        .remove(&id)
        .ok_or("application is not registered")?;
    Ok((id, manifest))
}

fn consume_launch_descriptor(id: &AppId, schema: u32) -> io::Result<LaunchDescriptor> {
    let path = Path::new(LAUNCH_ROOT).join(format!("{}.json", id.as_str()));
    let descriptor = read_launch_descriptor(&path, schema)?;
    let _ = fs::remove_file(path);
    Ok(descriptor)
}

fn lifecycle_requested(manifest: &AppManifest) -> bool {
    manifest.schema == MANIFEST_SCHEMA_V2
        && manifest
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == "lifecycle:v2")
}

fn prepare_lifecycle(
    id: &AppId,
    manifest: &AppManifest,
    launch: &LaunchDescriptor,
    _schema_ready: &data_schema::SchemaReady,
    plan: &mut ExecutionPlan,
) -> Result<LifecycleResources, Box<dyn std::error::Error>> {
    if !lifecycle_requested(manifest) {
        return Ok(LifecycleResources {
            bridge: None,
            child_descriptor: None,
            control_socket: None,
            status_store: None,
        });
    }
    let (parent, child) = lifecycle_socket_pair()?;
    let generation = plan
        .generation
        .ok_or("lifecycle launch has no generation")?;
    let foreground_epoch = launch
        .foreground_epoch
        .filter(|value| *value != 0)
        .ok_or("lifecycle launch has no foreground epoch")?;
    let lease_id = launch
        .lease_id
        .filter(|value| *value != 0)
        .ok_or("lifecycle launch has no display lease")?;
    let bridge = LifecycleBridge::new(
        parent,
        id.clone(),
        generation,
        foreground_epoch,
        lease_id,
        manifest.shutdown.graceful_timeout_ms,
    )?;
    let runtime_dir = plan
        .launch_environment
        .as_ref()
        .ok_or("lifecycle launch has no resolved environment")?
        .directories
        .runtime_dir
        .clone();
    let status_store = LifecycleStatusStore::new(runtime_dir.clone());
    status_store.clear_stale()?;
    let control_socket = ControlSocket::bind(runtime_dir.join("control.sock"))?;
    inject_runtime_variable(plan, "REMAGIC_LIFECYCLE_FD", child.as_raw_fd().to_string())?;
    inject_runtime_variable(
        plan,
        "REMAGIC_APP_CONTROL_SOCKET",
        control_socket.path().display().to_string(),
    )?;
    Ok(LifecycleResources {
        bridge: Some(bridge),
        child_descriptor: Some(child),
        control_socket: Some(control_socket),
        status_store: Some(status_store),
    })
}

fn inject_runtime_variable(
    plan: &mut ExecutionPlan,
    key: &str,
    value: String,
) -> Result<(), Box<dyn std::error::Error>> {
    plan.variables.insert(key.to_owned(), value.clone());
    if let Some(environment) = &mut plan.launch_environment {
        environment.variables.insert(key.to_owned(), value);
        environment.validate()?;
    }
    Ok(())
}

pub(crate) fn read_launch_descriptor(path: &Path, schema: u32) -> io::Result<LaunchDescriptor> {
    let bytes = match fs::File::open(path) {
        Ok(file) if file.metadata()?.len() > MAX_LAUNCH_DESCRIPTOR => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "launch descriptor exceeds 1 MiB",
            ));
        }
        Ok(_) => fs::read(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LaunchDescriptor::default());
        }
        Err(error) => return Err(error),
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => Ok(value),
        Err(_) if schema != MANIFEST_SCHEMA_V2 => Ok(LaunchDescriptor::default()),
        Err(error) => Err(io::Error::new(io::ErrorKind::InvalidData, error)),
    }
}

pub(crate) fn lifecycle_socket_pair() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut descriptors = [-1; 2];
    let result = unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
            descriptors.as_mut_ptr(),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socketpair returned two new owned descriptors.
    Ok(unsafe {
        (
            OwnedFd::from_raw_fd(descriptors[0]),
            OwnedFd::from_raw_fd(descriptors[1]),
        )
    })
}

use std::os::fd::AsRawFd;

#[cfg(test)]
mod tests;
