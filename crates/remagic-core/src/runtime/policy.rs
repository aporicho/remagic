use super::{RuntimeProfile, RuntimeValidationError};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Concrete filesystem roots assigned to one application.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeDirectories {
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub data_home: PathBuf,
    pub state_home: PathBuf,
    pub cache_home: PathBuf,
    pub runtime_dir: PathBuf,
}

impl RuntimeDirectories {
    pub fn validate(&self) -> Result<(), RuntimeValidationError> {
        let paths = [
            ("home", &self.home),
            ("config_home", &self.config_home),
            ("data_home", &self.data_home),
            ("state_home", &self.state_home),
            ("cache_home", &self.cache_home),
            ("runtime_dir", &self.runtime_dir),
        ];
        for (field, value) in paths {
            validate_absolute(field, value)?;
            if value == Path::new("/") {
                return Err(RuntimeValidationError::UnsafePath(
                    field,
                    value.to_path_buf(),
                ));
            }
        }

        let mut unique = BTreeSet::new();
        for (field, value) in paths {
            if !unique.insert(value) {
                return Err(RuntimeValidationError::DuplicatePath(
                    field,
                    value.to_path_buf(),
                ));
            }
        }

        for (field, value) in [
            ("config_home", &self.config_home),
            ("data_home", &self.data_home),
            ("state_home", &self.state_home),
            ("cache_home", &self.cache_home),
        ] {
            if value == &self.home || !value.starts_with(&self.home) {
                return Err(RuntimeValidationError::DirectoryOutsideHome(
                    field,
                    value.to_path_buf(),
                    self.home.clone(),
                ));
            }
        }
        if self.runtime_dir.starts_with(&self.home) {
            return Err(RuntimeValidationError::PersistentRuntimeDirectory(
                self.runtime_dir.clone(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LocalePolicy {
    #[serde(default = "default_locale")]
    pub lang: String,
    #[serde(default)]
    pub lc_all: Option<String>,
}

impl Default for LocalePolicy {
    fn default() -> Self {
        Self {
            lang: default_locale(),
            lc_all: None,
        }
    }
}

fn default_locale() -> String {
    "C.UTF-8".to_owned()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimezonePolicy {
    #[serde(default = "default_timezone")]
    pub name: String,
}

impl Default for TimezonePolicy {
    fn default() -> Self {
        Self {
            name: default_timezone(),
        }
    }
}

fn default_timezone() -> String {
    "UTC".to_owned()
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FontPolicy {
    #[serde(default)]
    pub directories: Vec<PathBuf>,
    #[serde(default)]
    pub fontconfig_file: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CertificatePolicy {
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub ca_bundle: Option<PathBuf>,
    #[serde(default)]
    pub ca_directory: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    #[default]
    Deny,
    HttpsOnly,
    Outbound,
    /// Accept connections on a platform-assigned local address. This grants
    /// no outbound-network contract.
    Inbound,
}

impl NetworkMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::HttpsOnly => "https_only",
            Self::Outbound => "outbound",
            Self::Inbound => "inbound",
        }
    }
}

/// Describes what the platform actually did with a requested network policy.
///
/// `MetadataOnly` is deliberately explicit: applications can inspect the
/// requested policy, but the process still has the host's ordinary network
/// namespace. It must never be presented as network isolation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkEnforcement {
    #[default]
    MetadataOnly,
    Enforced,
}

/// Controls whether a resident application's cgroup keeps executing after it
/// has released its foreground display and input leases.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundExecution {
    /// Keep the resident process scheduled while it is in the background.
    #[default]
    Continue,
    /// Freeze the resident process cgroup until the next foreground recall.
    Freeze,
}

impl BackgroundExecution {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Freeze => "freeze",
        }
    }

    pub const fn freezes_process(self) -> bool {
        matches!(self, Self::Freeze)
    }
}

impl NetworkEnforcement {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MetadataOnly => "metadata_only",
            Self::Enforced => "enforced",
        }
    }

    pub const fn is_enforced(self) -> bool {
        matches!(self, Self::Enforced)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NetworkPolicy {
    #[serde(default)]
    pub mode: NetworkMode,
    /// Empty means all destinations allowed by `mode`; values are DNS names,
    /// optionally beginning with `*.` for a subdomain suffix.
    #[serde(default)]
    pub allowed_hosts: BTreeSet<String>,
    /// Required for `inbound`; forbidden for every other mode. ReMagic owns
    /// the bind host and exposes the resulting address in the launch
    /// environment, while the application selects only its non-privileged
    /// port.
    #[serde(default)]
    pub listen_port: Option<u16>,
    /// Refuse to launch unless the platform can prove that it installed an
    /// operating-system enforcement boundary for this policy.
    #[serde(default)]
    pub require_enforcement: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeRequirements {
    #[serde(default)]
    pub profile: RuntimeProfile,
    /// SONAMEs (not paths) that must be resolvable before launch.
    #[serde(default)]
    pub required_libraries: Vec<String>,
    /// Required in schema v2. Optional here so schema v1 remains readable.
    #[serde(default)]
    pub directories: Option<RuntimeDirectories>,
    #[serde(default)]
    pub locale: LocalePolicy,
    #[serde(default)]
    pub timezone: TimezonePolicy,
    #[serde(default)]
    pub fonts: FontPolicy,
    #[serde(default)]
    pub certificates: CertificatePolicy,
    #[serde(default)]
    pub network: NetworkPolicy,
    /// Scheduling policy after a resident application has completed its
    /// semantic background transition. Defaults to continued execution for
    /// compatibility with existing schema-v2 manifests.
    #[serde(default)]
    pub background_execution: BackgroundExecution,
}

impl RuntimeRequirements {
    pub fn validate(&self, require_directories: bool) -> Result<(), RuntimeValidationError> {
        validate_directories(self.directories.as_ref(), require_directories)?;
        validate_libraries(&self.required_libraries)?;
        validate_locale("locale.lang", &self.locale.lang)?;
        if let Some(lc_all) = &self.locale.lc_all {
            validate_locale("locale.lc_all", lc_all)?;
        }
        validate_timezone(&self.timezone.name)?;
        validate_font_policy(&self.fonts)?;
        validate_certificate_policy(&self.certificates)?;
        validate_network_policy(&self.network)
    }
}

fn validate_directories(
    directories: Option<&RuntimeDirectories>,
    required: bool,
) -> Result<(), RuntimeValidationError> {
    match directories {
        Some(directories) => directories.validate(),
        None if required => Err(RuntimeValidationError::MissingDirectories),
        None => Ok(()),
    }
}

fn validate_libraries(libraries: &[String]) -> Result<(), RuntimeValidationError> {
    let mut seen = BTreeSet::new();
    for library in libraries {
        let valid = !library.is_empty()
            && library.len() <= 128
            && !library.contains('/')
            && library.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
            });
        if !valid {
            return Err(RuntimeValidationError::InvalidLibrary(library.clone()));
        }
        if !seen.insert(library) {
            return Err(RuntimeValidationError::DuplicateLibrary(library.clone()));
        }
    }
    Ok(())
}

fn validate_font_policy(fonts: &FontPolicy) -> Result<(), RuntimeValidationError> {
    let mut font_directories = BTreeSet::new();
    for directory in &fonts.directories {
        validate_absolute("fonts.directories", directory)?;
        if !font_directories.insert(directory) {
            return Err(RuntimeValidationError::DuplicatePath(
                "fonts.directories",
                directory.clone(),
            ));
        }
    }
    if let Some(file) = &fonts.fontconfig_file {
        validate_absolute("fonts.fontconfig_file", file)?;
    }
    Ok(())
}

fn validate_certificate_policy(
    certificates: &CertificatePolicy,
) -> Result<(), RuntimeValidationError> {
    if let Some(bundle) = &certificates.ca_bundle {
        validate_absolute("certificates.ca_bundle", bundle)?;
    }
    if let Some(directory) = &certificates.ca_directory {
        validate_absolute("certificates.ca_directory", directory)?;
    }
    if certificates.required
        && certificates.ca_bundle.is_none()
        && certificates.ca_directory.is_none()
    {
        return Err(RuntimeValidationError::MissingCertificateSource);
    }
    Ok(())
}

fn validate_network_policy(network: &NetworkPolicy) -> Result<(), RuntimeValidationError> {
    if matches!(network.mode, NetworkMode::Deny | NetworkMode::Inbound)
        && !network.allowed_hosts.is_empty()
    {
        return Err(RuntimeValidationError::HostsWithDeniedNetwork);
    }
    match (network.mode, network.listen_port) {
        (NetworkMode::Inbound, Some(port)) if port >= 1024 => {}
        (NetworkMode::Inbound, _) => return Err(RuntimeValidationError::MissingListenPort),
        (_, Some(port)) => return Err(RuntimeValidationError::UnexpectedListenPort(port)),
        (_, None) => {}
    }
    for host in &network.allowed_hosts {
        validate_host(host)?;
    }
    Ok(())
}

pub(super) fn validate_absolute(
    field: &'static str,
    value: &Path,
) -> Result<(), RuntimeValidationError> {
    if !value.is_absolute()
        || value.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(RuntimeValidationError::UnsafePath(
            field,
            value.to_path_buf(),
        ));
    }
    Ok(())
}

fn validate_locale(field: &'static str, value: &str) -> Result<(), RuntimeValidationError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'@'))
    {
        return Err(RuntimeValidationError::InvalidPolicyValue(
            field,
            value.to_owned(),
        ));
    }
    Ok(())
}

fn validate_timezone(value: &str) -> Result<(), RuntimeValidationError> {
    if value.is_empty()
        || value.len() > 128
        || value.starts_with('/')
        || value.contains("..")
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'_' | b'-' | b'+'))
    {
        return Err(RuntimeValidationError::InvalidPolicyValue(
            "timezone.name",
            value.to_owned(),
        ));
    }
    Ok(())
}

fn validate_host(host: &str) -> Result<(), RuntimeValidationError> {
    let host = host.strip_prefix("*.").unwrap_or(host);
    let valid = !host.is_empty()
        && host.len() <= 253
        && !host.starts_with('.')
        && !host.ends_with('.')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    if valid {
        Ok(())
    } else {
        Err(RuntimeValidationError::InvalidHost(host.to_owned()))
    }
}

pub(super) fn encode_path_list(paths: &[PathBuf]) -> Result<String, RuntimeValidationError> {
    std::env::join_paths(paths)
        .map_err(|error| RuntimeValidationError::InvalidPathList(error.to_string()))?
        .into_string()
        .map_err(|_| RuntimeValidationError::InvalidPathList("path is not UTF-8".into()))
}
