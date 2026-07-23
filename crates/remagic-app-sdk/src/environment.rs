use remagic_core::AppId;
use remagic_device::{DeviceProfile, DEVICE_PROFILE_ENV};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Clone, Debug)]
pub struct ManagedEnvironment {
    pub app_id: AppId,
    pub device: DeviceProfile,
    pub qtfb_key: i32,
    pub qtfb_socket: PathBuf,
    pub lifecycle_fd: i32,
    pub listen_addr: Option<SocketAddr>,
    pub books_dir: Option<PathBuf>,
    pub wallpapers_dir: Option<PathBuf>,
}

impl ManagedEnvironment {
    pub fn discover(expected_app_id: &str) -> Result<Self, ManagedEnvironmentError> {
        if env::var("REMAGIC_MANAGED").as_deref() != Ok("1") {
            return Err(ManagedEnvironmentError::NotManaged);
        }
        let app_id = AppId::new(required("REMAGIC_APP_ID")?).map_err(|error| {
            ManagedEnvironmentError::Invalid("REMAGIC_APP_ID", error.to_string())
        })?;
        if app_id.as_str() != expected_app_id {
            return Err(ManagedEnvironmentError::AppMismatch {
                expected: expected_app_id.to_owned(),
                actual: app_id.to_string(),
            });
        }
        let device: DeviceProfile =
            serde_json::from_str(&required(DEVICE_PROFILE_ENV)?).map_err(|error| {
                ManagedEnvironmentError::Invalid(DEVICE_PROFILE_ENV, error.to_string())
            })?;
        device.validate().map_err(|error| {
            ManagedEnvironmentError::Invalid(DEVICE_PROFILE_ENV, error.to_string())
        })?;

        Ok(Self {
            app_id,
            device,
            qtfb_key: parse("QTFB_KEY")?,
            qtfb_socket: PathBuf::from(required("REMAGIC_QTFB_SOCKET")?),
            lifecycle_fd: parse("REMAGIC_LIFECYCLE_FD")?,
            listen_addr: optional_parse("REMAGIC_LISTEN_ADDR")?,
            books_dir: optional_path("REMAGIC_BOOKS_DIR"),
            wallpapers_dir: optional_path("REMAGIC_WALLPAPERS_DIR"),
        })
    }

    pub fn require_upload_contract(&self) -> Result<(), ManagedEnvironmentError> {
        for capability in [
            "display:qtfb-v1",
            "input:touch-v1",
            "lifecycle:v2",
            "network:listen-v1",
            "storage:books-write-v1",
            "storage:wallpapers-write-v1",
        ] {
            if !self
                .device
                .capabilities
                .iter()
                .any(|value| value == capability)
            {
                return Err(ManagedEnvironmentError::MissingCapability(capability));
            }
        }
        if self.listen_addr.is_none() || self.books_dir.is_none() || self.wallpapers_dir.is_none() {
            return Err(ManagedEnvironmentError::IncompleteUploadContract);
        }
        Ok(())
    }
}

fn required(name: &'static str) -> Result<String, ManagedEnvironmentError> {
    env::var(name).map_err(|_| ManagedEnvironmentError::Missing(name))
}

fn parse<T>(name: &'static str) -> Result<T, ManagedEnvironmentError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    required(name)?
        .parse()
        .map_err(|error: T::Err| ManagedEnvironmentError::Invalid(name, error.to_string()))
}

fn optional_parse<T>(name: &'static str) -> Result<Option<T>, ManagedEnvironmentError>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match env::var(name) {
        Ok(value) if !value.is_empty() => value
            .parse()
            .map(Some)
            .map_err(|error: T::Err| ManagedEnvironmentError::Invalid(name, error.to_string())),
        _ => Ok(None),
    }
}

fn optional_path(name: &'static str) -> Option<PathBuf> {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[derive(Debug, Error)]
pub enum ManagedEnvironmentError {
    #[error("application was not launched by ReMagic")]
    NotManaged,
    #[error("managed environment is missing {0}")]
    Missing(&'static str),
    #[error("invalid {0}: {1}")]
    Invalid(&'static str, String),
    #[error("runtime launched app {actual}, expected {expected}")]
    AppMismatch { expected: String, actual: String },
    #[error("device profile is missing required capability {0}")]
    MissingCapability(&'static str),
    #[error("runtime did not provide the upload network and storage contract")]
    IncompleteUploadContract,
}
