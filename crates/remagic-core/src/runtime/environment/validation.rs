use super::*;
use crate::runtime::policy::{encode_path_list, validate_absolute};

impl LaunchEnvironment {
    pub fn validate(&self) -> Result<(), RuntimeValidationError> {
        self.directories.validate()?;
        validate_environment_variables(&self.variables)?;
        validate_directory_variables(self)?;
        validate_network_variables(self)?;
        validate_identity_variables(self)?;
        validate_locale_variables(self)?;
        validate_font_variables(self)?;
        validate_certificate_variables(self)?;
        validate_library_paths(&self.resolved_libraries)?;
        RuntimeRequirements {
            profile: self.profile,
            required_libraries: Vec::new(),
            directories: Some(self.directories.clone()),
            locale: self.locale.clone(),
            timezone: self.timezone.clone(),
            fonts: self.fonts.clone(),
            certificates: self.certificates.clone(),
            network: self.network.clone(),
        }
        .validate(true)
    }
}

fn validate_environment_variables(
    variables: &BTreeMap<String, String>,
) -> Result<(), RuntimeValidationError> {
    for (key, value) in variables {
        validate_environment_pair(key, value)?;
    }
    Ok(())
}

fn validate_directory_variables(
    environment: &LaunchEnvironment,
) -> Result<(), RuntimeValidationError> {
    for (key, expected) in [
        ("HOME", &environment.directories.home),
        ("XDG_CONFIG_HOME", &environment.directories.config_home),
        ("XDG_DATA_HOME", &environment.directories.data_home),
        ("XDG_STATE_HOME", &environment.directories.state_home),
        ("XDG_CACHE_HOME", &environment.directories.cache_home),
        ("XDG_RUNTIME_DIR", &environment.directories.runtime_dir),
    ] {
        let actual = required_variable(&environment.variables, key)?;
        if Path::new(actual) != expected {
            return Err(RuntimeValidationError::MismatchedLaunchVariable(
                key,
                expected.clone(),
                actual.to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_network_variables(
    environment: &LaunchEnvironment,
) -> Result<(), RuntimeValidationError> {
    let allowed_hosts = environment
        .network
        .allowed_hosts
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(",");
    validate_exact_variable(
        &environment.variables,
        "REMAGIC_NETWORK_POLICY_MODE",
        environment.network.mode.as_str(),
    )?;
    validate_exact_variable(
        &environment.variables,
        "REMAGIC_NETWORK_ALLOWED_HOSTS",
        &allowed_hosts,
    )?;
    let enforcement =
        required_variable(&environment.variables, "REMAGIC_NETWORK_POLICY_ENFORCEMENT")?;
    let isolated = required_variable(&environment.variables, "REMAGIC_NETWORK_ISOLATED")?;
    let enforced = match (enforcement, isolated) {
        ("metadata_only", "0") => false,
        ("enforced", "1") => true,
        _ => return Err(policy_mismatch("REMAGIC_NETWORK_POLICY_ENFORCEMENT")),
    };
    if environment.network.require_enforcement && !enforced {
        return Err(RuntimeValidationError::RequiredNetworkEnforcementUnavailable);
    }
    Ok(())
}

fn validate_identity_variables(
    environment: &LaunchEnvironment,
) -> Result<(), RuntimeValidationError> {
    validate_exact_variable(
        &environment.variables,
        "REMAGIC_APP_ID",
        environment.app_id.as_str(),
    )?;
    validate_exact_variable(
        &environment.variables,
        "REMAGIC_RUNTIME_PROFILE",
        environment.profile.as_str(),
    )?;
    validate_platform_path(&environment.variables)?;
    let qtfb_key = environment.variables.get("QTFB_KEY");
    let expected = qtfb_key_for_app(&environment.app_id).to_string();
    match environment.profile {
        RuntimeProfile::QtfbCompat if qtfb_key.is_some_and(|actual| actual != &expected) => {
            Err(policy_mismatch("QTFB_KEY"))
        }
        RuntimeProfile::NativeV2 | RuntimeProfile::Headless if qtfb_key.is_some() => {
            Err(policy_mismatch("QTFB_KEY"))
        }
        _ => Ok(()),
    }
}

fn validate_platform_path(
    variables: &BTreeMap<String, String>,
) -> Result<(), RuntimeValidationError> {
    let path = required_variable(variables, "PATH")?;
    let invalid = path.is_empty()
        || path.split(':').any(|entry| {
            let entry = Path::new(entry);
            !entry.is_absolute()
                || entry
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
        });
    if invalid {
        return Err(RuntimeValidationError::InvalidPolicyValue(
            "PATH",
            path.to_owned(),
        ));
    }
    Ok(())
}

fn validate_locale_variables(
    environment: &LaunchEnvironment,
) -> Result<(), RuntimeValidationError> {
    validate_exact_variable(&environment.variables, "LANG", &environment.locale.lang)?;
    validate_exact_variable(&environment.variables, "TZ", &environment.timezone.name)?;
    validate_optional_variable(
        &environment.variables,
        "LC_ALL",
        environment.locale.lc_all.as_deref(),
    )
}

fn validate_font_variables(environment: &LaunchEnvironment) -> Result<(), RuntimeValidationError> {
    let fontconfig_file = environment
        .fonts
        .fontconfig_file
        .as_ref()
        .map(|path| path.display().to_string());
    validate_optional_variable(
        &environment.variables,
        "FONTCONFIG_FILE",
        fontconfig_file.as_deref(),
    )?;
    let directories = if environment.fonts.directories.is_empty() {
        None
    } else {
        Some(encode_path_list(&environment.fonts.directories)?)
    };
    for key in ["QT_QPA_FONTDIR", "REMAGIC_FONT_DIRECTORIES"] {
        validate_optional_variable(&environment.variables, key, directories.as_deref())?;
    }
    Ok(())
}

fn validate_certificate_variables(
    environment: &LaunchEnvironment,
) -> Result<(), RuntimeValidationError> {
    let bundle = environment
        .certificates
        .ca_bundle
        .as_ref()
        .map(|path| path.display().to_string());
    let directory = environment
        .certificates
        .ca_directory
        .as_ref()
        .map(|path| path.display().to_string());
    validate_optional_variable(&environment.variables, "SSL_CERT_FILE", bundle.as_deref())?;
    validate_optional_variable(&environment.variables, "SSL_CERT_DIR", directory.as_deref())
}

fn validate_library_paths(libraries: &[PathBuf]) -> Result<(), RuntimeValidationError> {
    let mut unique = BTreeSet::new();
    for library in libraries {
        validate_absolute("resolved_libraries", library)?;
        if !unique.insert(library) {
            return Err(RuntimeValidationError::DuplicatePath(
                "resolved_libraries",
                library.clone(),
            ));
        }
    }
    Ok(())
}

fn required_variable<'a>(
    variables: &'a BTreeMap<String, String>,
    key: &'static str,
) -> Result<&'a str, RuntimeValidationError> {
    variables
        .get(key)
        .map(String::as_str)
        .ok_or(RuntimeValidationError::MissingLaunchVariable(key))
}

fn validate_exact_variable(
    variables: &BTreeMap<String, String>,
    key: &'static str,
    expected: &str,
) -> Result<(), RuntimeValidationError> {
    if required_variable(variables, key)? == expected {
        Ok(())
    } else {
        Err(policy_mismatch(key))
    }
}

fn validate_optional_variable(
    variables: &BTreeMap<String, String>,
    key: &'static str,
    expected: Option<&str>,
) -> Result<(), RuntimeValidationError> {
    if variables.get(key).map(String::as_str) == expected {
        Ok(())
    } else {
        Err(policy_mismatch(key))
    }
}

fn policy_mismatch(key: &'static str) -> RuntimeValidationError {
    RuntimeValidationError::PolicyVariableMismatch(key)
}

pub fn is_platform_reserved_environment(key: &str) -> bool {
    key == "PATH"
        || key == "HOME"
        || key == "LANG"
        || key == "LC_ALL"
        || key == "TZ"
        || key == "LD_LIBRARY_PATH"
        || key == "SSL_CERT_FILE"
        || key == "SSL_CERT_DIR"
        || key == "FONTCONFIG_FILE"
        || key == "QT_QPA_PLATFORM"
        || key == "QT_QPA_FONTDIR"
        || key == "QTFB_KEY"
        || key.starts_with("XDG_")
        || key.starts_with("REMAGIC_")
}

pub fn validate_environment_pair(key: &str, value: &str) -> Result<(), RuntimeValidationError> {
    let mut bytes = key.bytes();
    let first = bytes.next();
    let valid_key = first.is_some_and(|byte| byte.is_ascii_uppercase() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_');
    if !valid_key || key.len() > 128 || value.as_bytes().contains(&0) {
        return Err(RuntimeValidationError::InvalidEnvironment(key.to_owned()));
    }
    Ok(())
}
