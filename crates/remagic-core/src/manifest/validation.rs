use super::*;
use crate::runtime::{is_platform_reserved_environment, validate_environment_pair};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const RESERVED_ENV: &[&str] = &[
    "PATH",
    "REMAGIC_SOCKET",
    "REMAGIC_APP_ID",
    "REMAGIC_LAUNCH_ID",
];

impl AppManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        self.validate_identity_and_paths()?;
        self.validate_environment()?;
        self.validate_arguments_and_capabilities()?;
        self.validate_compatibility()?;
        self.validate_readiness()?;
        self.validate_shutdown()?;
        self.validate_background_service()?;
        self.validate_data_schema()?;
        self.runtime
            .validate(self.schema == MANIFEST_SCHEMA_V2)
            .map_err(ManifestError::Runtime)?;
        self.validate_background_execution()?;
        if self.schema == MANIFEST_SCHEMA_V2 {
            self.validate_v2_fields()?;
        }
        Ok(())
    }

    fn validate_background_execution(&self) -> Result<(), ManifestError> {
        if !self.runtime.background_execution.freezes_process() {
            return Ok(());
        }
        if self.schema != MANIFEST_SCHEMA_V2 {
            return Err(ManifestError::FreezeRequiresV2);
        }
        if !self.resident {
            return Err(ManifestError::FreezeRequiresResident);
        }
        if !self
            .capabilities
            .iter()
            .any(|capability| capability.as_str() == "lifecycle:v2")
        {
            return Err(ManifestError::FreezeRequiresLifecycleV2);
        }
        Ok(())
    }

    fn validate_identity_and_paths(&self) -> Result<(), ManifestError> {
        if !matches!(self.schema, MANIFEST_SCHEMA_V1 | MANIFEST_SCHEMA_V2) {
            return Err(ManifestError::UnsupportedSchema(self.schema));
        }
        if self.name.trim().is_empty() {
            return Err(ManifestError::EmptyName(self.id.to_string()));
        }
        validate_absolute("exec", &self.exec)?;
        validate_absolute("working_dir", &self.working_dir)?;
        if let Some(icon) = &self.icon {
            validate_absolute("icon", icon)?;
        }
        for root in &self.allowed_open_roots {
            validate_absolute("allowed_open_roots", root)?;
        }
        if !matches!(
            self.display.as_str(),
            "qtfb" | "quill" | "einkface" | "none"
        ) {
            return Err(ManifestError::UnsupportedDisplay(self.display.clone()));
        }
        Ok(())
    }

    fn validate_environment(&self) -> Result<(), ManifestError> {
        for (key, value) in &self.environment {
            if key.is_empty()
                || key.contains('=')
                || key.bytes().any(|b| b == 0)
                || RESERVED_ENV.contains(&key.as_str())
                || value.bytes().any(|b| b == 0)
                || (self.schema == MANIFEST_SCHEMA_V2
                    && (is_platform_reserved_environment(key.as_str())
                        || validate_environment_pair(key, value).is_err()))
            {
                return Err(ManifestError::UnsafeEnvironment(key.clone()));
            }
        }
        Ok(())
    }

    fn validate_arguments_and_capabilities(&self) -> Result<(), ManifestError> {
        if self
            .args
            .iter()
            .any(|arg| arg.bytes().any(|byte| byte == 0))
        {
            return Err(ManifestError::InvalidArgument);
        }
        let mut capabilities = BTreeSet::new();
        for capability in &self.capabilities {
            if !capabilities.insert(capability) {
                return Err(ManifestError::DuplicateCapability(capability.to_string()));
            }
        }
        Ok(())
    }

    fn validate_compatibility(&self) -> Result<(), ManifestError> {
        if !(1..=REMAGIC_APP_API_VERSION).contains(&self.required_remagic_api) {
            return Err(ManifestError::UnsupportedRemagicApi(
                self.required_remagic_api,
            ));
        }
        let mut devices = BTreeSet::new();
        for product in &self.supported_devices {
            if !devices.insert(*product) {
                return Err(ManifestError::DuplicateSupportedDevice(*product));
            }
        }
        let mut versions = BTreeSet::new();
        for version in &self.supported_os {
            if version.trim().is_empty() || !versions.insert(version) {
                return Err(ManifestError::InvalidSupportedOs(version.clone()));
            }
        }
        Ok(())
    }

    fn validate_v2_fields(&self) -> Result<(), ManifestError> {
        if self.park_strategy == ParkStrategy::Resident && !self.resident {
            return Err(ManifestError::ConflictingResidentPolicy);
        }
        if self.background_unit.is_some() {
            return Err(ManifestError::LegacyFieldInV2("background_unit"));
        }
        self.validate_v2_runtime_contract()
    }

    pub fn is_resident(&self) -> bool {
        if self.schema == MANIFEST_SCHEMA_V1 {
            self.park_strategy == ParkStrategy::Resident
        } else {
            self.resident
        }
    }

    pub fn runtime_profile(&self) -> RuntimeProfile {
        if self.schema == MANIFEST_SCHEMA_V1 {
            match self.display.as_str() {
                "qtfb" | "einkface" => RuntimeProfile::QtfbCompat,
                "none" => RuntimeProfile::Headless,
                _ => RuntimeProfile::NativeV2,
            }
        } else {
            self.runtime.profile
        }
    }

    pub fn effective_background_service(&self) -> Option<BackgroundService> {
        self.background_service.clone().or_else(|| {
            self.background_unit
                .clone()
                .map(|unit| BackgroundService::Systemd { unit })
        })
    }

    pub fn validate_open_path(&self, path: &Path) -> Result<PathBuf, ManifestError> {
        if !self.supports_open_path {
            return Err(ManifestError::OpenPathUnsupported(self.id.to_string()));
        }
        let canonical = path
            .canonicalize()
            .map_err(|source| ManifestError::Canonicalize(path.to_path_buf(), source))?;
        let allowed = self.allowed_open_roots.iter().any(|root| {
            root.canonicalize()
                .map(|root| canonical == root || canonical.starts_with(&root))
                .unwrap_or(false)
        });
        if allowed {
            Ok(canonical)
        } else {
            Err(ManifestError::OpenPathDenied(canonical))
        }
    }

    fn validate_readiness(&self) -> Result<(), ManifestError> {
        if !(100..=120_000).contains(&self.readiness.timeout_ms) {
            return Err(ManifestError::InvalidReadinessTimeout(
                self.readiness.timeout_ms,
            ));
        }
        match (&self.readiness.mode, &self.readiness.path) {
            (ReadinessMode::File, Some(path)) => validate_absolute("readiness.path", path),
            (ReadinessMode::File, None) => Err(ManifestError::MissingReadinessPath),
            (_, Some(_)) => Err(ManifestError::UnexpectedReadinessPath),
            (_, None) => Ok(()),
        }
    }

    fn validate_shutdown(&self) -> Result<(), ManifestError> {
        let policy = &self.shutdown;
        if policy.graceful_timeout_ms < 100
            || policy.graceful_timeout_ms > policy.term_timeout_ms
            || policy.term_timeout_ms > policy.kill_timeout_ms
            || policy.kill_timeout_ms > MAX_SHUTDOWN_KILL_TIMEOUT_MS
        {
            return Err(ManifestError::InvalidShutdownPolicy {
                graceful_timeout_ms: policy.graceful_timeout_ms,
                term_timeout_ms: policy.term_timeout_ms,
                kill_timeout_ms: policy.kill_timeout_ms,
            });
        }
        Ok(())
    }

    fn validate_background_service(&self) -> Result<(), ManifestError> {
        let Some(service) = &self.background_service else {
            return Ok(());
        };
        match service {
            BackgroundService::Managed {
                exec,
                args,
                working_dir,
                ..
            } => {
                validate_absolute("background_service.exec", exec)?;
                validate_absolute("background_service.working_dir", working_dir)?;
                if args
                    .iter()
                    .any(|argument| argument.bytes().any(|byte| byte == 0))
                {
                    return Err(ManifestError::InvalidArgument);
                }
            }
            BackgroundService::Systemd { unit } => {
                let valid = !unit.is_empty()
                    && unit.len() <= 128
                    && unit.ends_with(".service")
                    && unit.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'@')
                    });
                if !valid {
                    return Err(ManifestError::InvalidSystemdUnit(unit.clone()));
                }
            }
        }
        Ok(())
    }

    fn validate_data_schema(&self) -> Result<(), ManifestError> {
        let Some(schema) = &self.data_schema else {
            return Ok(());
        };
        if self.schema != MANIFEST_SCHEMA_V2 {
            return Err(ManifestError::DataSchemaRequiresV2);
        }
        if schema.version == 0 {
            return Err(ManifestError::InvalidDataSchemaVersion);
        }
        if !(100..=600_000).contains(&schema.backup_timeout_ms) {
            return Err(ManifestError::InvalidBackupTimeout(
                schema.backup_timeout_ms,
            ));
        }
        if !(100..=600_000).contains(&schema.migration_timeout_ms) {
            return Err(ManifestError::InvalidMigrationTimeout(
                schema.migration_timeout_ms,
            ));
        }
        if let Some(migrator) = &schema.migrator {
            validate_absolute("data_schema.migrator", migrator)?;
        }
        let mut backup_paths = BTreeSet::new();
        for path in &schema.backup_paths {
            validate_absolute("data_schema.backup_paths", path)?;
            if !backup_paths.insert(path) {
                return Err(ManifestError::DuplicateBackupPath(path.clone()));
            }
        }
        for (index, path) in schema.backup_paths.iter().enumerate() {
            for other in schema.backup_paths.iter().skip(index + 1) {
                if path.starts_with(other) || other.starts_with(path) {
                    return Err(ManifestError::OverlappingBackupPaths(
                        path.clone(),
                        other.clone(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_v2_runtime_contract(&self) -> Result<(), ManifestError> {
        let has_capability = |required: &str| {
            self.capabilities
                .iter()
                .any(|capability| capability.as_str() == required)
        };
        let (expected_display, required_capability) = match self.runtime.profile {
            RuntimeProfile::QtfbCompat => ("qtfb", Some("display:qtfb-v1")),
            RuntimeProfile::NativeV2 => ("quill", Some("display:surface-v2")),
            RuntimeProfile::Headless => ("none", None),
        };
        if self.display != expected_display {
            return Err(ManifestError::RuntimeDisplayMismatch {
                profile: self.runtime.profile,
                expected: expected_display,
                actual: self.display.clone(),
            });
        }
        if let Some(required) = required_capability {
            if !has_capability(required) {
                return Err(ManifestError::MissingRuntimeCapability {
                    profile: self.runtime.profile,
                    capability: required,
                });
            }
        } else if self
            .capabilities
            .iter()
            .any(|capability| capability.as_str().starts_with("display:"))
        {
            return Err(ManifestError::HeadlessDisplayCapability);
        }

        let declares_outbound = has_capability("network:outbound-v1");
        match self.runtime.network.mode {
            NetworkMode::Deny if declares_outbound => {
                return Err(ManifestError::NetworkCapabilityMismatch {
                    mode: NetworkMode::Deny,
                    expected: "no network:outbound-v1 capability",
                })
            }
            NetworkMode::HttpsOnly | NetworkMode::Outbound if !declares_outbound => {
                return Err(ManifestError::NetworkCapabilityMismatch {
                    mode: self.runtime.network.mode,
                    expected: "network:outbound-v1",
                })
            }
            _ => {}
        }
        Ok(())
    }
}

fn validate_absolute(field: &'static str, path: &Path) -> Result<(), ManifestError> {
    if !path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(ManifestError::UnsafePath(field, path.to_path_buf()));
    }
    Ok(())
}
