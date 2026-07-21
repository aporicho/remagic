use super::{
    qtfb_key_for_app, Capability, CertificatePolicy, FontPolicy, LocalePolicy, NetworkEnforcement,
    NetworkPolicy, RuntimeDirectories, RuntimeProfile, RuntimeRequirements, RuntimeValidationError,
    TimezonePolicy,
};
use crate::AppId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use super::policy::encode_path_list;

mod validation;

pub use validation::{is_platform_reserved_environment, validate_environment_pair};

/// Fully resolved, platform-owned process environment. Applications receive
/// this object in `Start`; they never get to override its reserved variables.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LaunchEnvironment {
    pub app_id: AppId,
    pub profile: RuntimeProfile,
    pub directories: RuntimeDirectories,
    pub variables: BTreeMap<String, String>,
    #[serde(default)]
    pub resolved_libraries: Vec<PathBuf>,
    #[serde(default)]
    pub platform_capabilities: BTreeSet<Capability>,
    pub locale: LocalePolicy,
    pub timezone: TimezonePolicy,
    pub fonts: FontPolicy,
    pub certificates: CertificatePolicy,
    pub network: NetworkPolicy,
}

impl LaunchEnvironment {
    #[allow(clippy::too_many_arguments)]
    pub fn resolve(
        app_id: AppId,
        requirements: &RuntimeRequirements,
        application_environment: &BTreeMap<String, String>,
        resolved_libraries: Vec<PathBuf>,
        platform_capabilities: BTreeSet<Capability>,
        platform_path: impl Into<String>,
        network_enforcement: NetworkEnforcement,
    ) -> Result<Self, RuntimeValidationError> {
        requirements.validate(true)?;
        let directories = requirements
            .directories
            .clone()
            .ok_or(RuntimeValidationError::MissingDirectories)?;
        validate_application_environment(application_environment)?;
        validate_required_libraries(requirements, &resolved_libraries)?;

        let mut variables = base_variables(
            &app_id,
            requirements,
            &directories,
            application_environment,
            platform_path.into(),
        );
        insert_network_variables(&mut variables, requirements, network_enforcement)?;
        insert_optional_policy_variables(&mut variables, requirements)?;
        insert_loader_variables(&mut variables, &app_id, requirements, &resolved_libraries);

        let environment = Self {
            app_id,
            profile: requirements.profile,
            directories,
            variables,
            resolved_libraries,
            platform_capabilities,
            locale: requirements.locale.clone(),
            timezone: requirements.timezone.clone(),
            fonts: requirements.fonts.clone(),
            certificates: requirements.certificates.clone(),
            network: requirements.network.clone(),
        };
        environment.validate()?;
        Ok(environment)
    }
}

fn validate_application_environment(
    application_environment: &BTreeMap<String, String>,
) -> Result<(), RuntimeValidationError> {
    for (key, value) in application_environment {
        validate_environment_pair(key, value)?;
        if is_platform_reserved_environment(key) {
            return Err(RuntimeValidationError::ReservedApplicationEnvironment(
                key.clone(),
            ));
        }
    }
    Ok(())
}

fn validate_required_libraries(
    requirements: &RuntimeRequirements,
    resolved_libraries: &[PathBuf],
) -> Result<(), RuntimeValidationError> {
    for required in &requirements.required_libraries {
        let is_resolved = resolved_libraries.iter().any(|path| {
            path.file_name()
                .is_some_and(|file_name| file_name == required.as_str())
        });
        if !is_resolved {
            return Err(RuntimeValidationError::UnresolvedLibrary(required.clone()));
        }
    }
    Ok(())
}

fn base_variables(
    app_id: &AppId,
    requirements: &RuntimeRequirements,
    directories: &RuntimeDirectories,
    application_environment: &BTreeMap<String, String>,
    platform_path: String,
) -> BTreeMap<String, String> {
    let mut variables = application_environment.clone();
    for (key, value) in [
        ("HOME", &directories.home),
        ("XDG_CONFIG_HOME", &directories.config_home),
        ("XDG_DATA_HOME", &directories.data_home),
        ("XDG_STATE_HOME", &directories.state_home),
        ("XDG_CACHE_HOME", &directories.cache_home),
        ("XDG_RUNTIME_DIR", &directories.runtime_dir),
    ] {
        variables.insert(key.into(), value.display().to_string());
    }
    variables.insert("LANG".into(), requirements.locale.lang.clone());
    variables.insert("TZ".into(), requirements.timezone.name.clone());
    variables.insert("PATH".into(), platform_path);
    variables.insert("REMAGIC_APP_ID".into(), app_id.to_string());
    variables.insert(
        "REMAGIC_RUNTIME_PROFILE".into(),
        requirements.profile.as_str().into(),
    );
    variables
}

fn insert_network_variables(
    variables: &mut BTreeMap<String, String>,
    requirements: &RuntimeRequirements,
    network_enforcement: NetworkEnforcement,
) -> Result<(), RuntimeValidationError> {
    if requirements.network.require_enforcement && !network_enforcement.is_enforced() {
        return Err(RuntimeValidationError::RequiredNetworkEnforcementUnavailable);
    }
    for (key, value) in [
        (
            "REMAGIC_NETWORK_POLICY_MODE",
            requirements.network.mode.as_str().to_owned(),
        ),
        (
            "REMAGIC_NETWORK_POLICY_ENFORCEMENT",
            network_enforcement.as_str().to_owned(),
        ),
        (
            "REMAGIC_NETWORK_ISOLATED",
            if network_enforcement.is_enforced() {
                "1"
            } else {
                "0"
            }
            .to_owned(),
        ),
        (
            "REMAGIC_NETWORK_ALLOWED_HOSTS",
            requirements
                .network
                .allowed_hosts
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join(","),
        ),
    ] {
        variables.insert(key.into(), value);
    }
    Ok(())
}

fn insert_optional_policy_variables(
    variables: &mut BTreeMap<String, String>,
    requirements: &RuntimeRequirements,
) -> Result<(), RuntimeValidationError> {
    if let Some(lc_all) = &requirements.locale.lc_all {
        variables.insert("LC_ALL".into(), lc_all.clone());
    }
    if let Some(file) = &requirements.fonts.fontconfig_file {
        variables.insert("FONTCONFIG_FILE".into(), file.display().to_string());
    }
    if !requirements.fonts.directories.is_empty() {
        let font_directories = encode_path_list(&requirements.fonts.directories)?;
        variables.insert("QT_QPA_FONTDIR".into(), font_directories.clone());
        variables.insert("REMAGIC_FONT_DIRECTORIES".into(), font_directories);
    }
    if let Some(bundle) = &requirements.certificates.ca_bundle {
        variables.insert("SSL_CERT_FILE".into(), bundle.display().to_string());
    }
    if let Some(directory) = &requirements.certificates.ca_directory {
        variables.insert("SSL_CERT_DIR".into(), directory.display().to_string());
    }
    Ok(())
}

fn insert_loader_variables(
    variables: &mut BTreeMap<String, String>,
    app_id: &AppId,
    requirements: &RuntimeRequirements,
    resolved_libraries: &[PathBuf],
) {
    if requirements.profile == RuntimeProfile::QtfbCompat {
        variables.insert("QTFB_KEY".into(), qtfb_key_for_app(app_id).to_string());
    }
    let library_directories: BTreeSet<_> = resolved_libraries
        .iter()
        .filter_map(|library| library.parent().map(Path::to_path_buf))
        .collect();
    if !library_directories.is_empty() {
        let library_path = library_directories
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(":");
        variables.insert("LD_LIBRARY_PATH".into(), library_path);
    }
}
