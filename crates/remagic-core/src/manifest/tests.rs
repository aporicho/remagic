use super::*;

fn legacy_manifest(environment: BTreeMap<String, String>) -> AppManifest {
    AppManifest {
        schema: MANIFEST_SCHEMA_V1,
        id: AppId::new("test").unwrap(),
        name: "Test".into(),
        kind: AppKind::User,
        description: String::new(),
        version: String::new(),
        icon: None,
        package: None,
        supported_devices: Vec::new(),
        supported_os: Vec::new(),
        required_remagic_api: 1,
        uninstall_policy: UninstallPolicy::KeepData,
        exec: "/bin/true".into(),
        args: vec![],
        working_dir: "/tmp".into(),
        display: "none".into(),
        park_strategy: ParkStrategy::Restart,
        resident: false,
        capabilities: Vec::new(),
        readiness: ReadinessPolicy::default(),
        shutdown: ShutdownPolicy::default(),
        background_service: None,
        data_schema: None,
        runtime: RuntimeRequirements::default(),
        background_unit: None,
        supports_open_path: false,
        allowed_open_roots: vec![],
        environment,
    }
}

#[test]
fn app_ids_are_strict() {
    assert!(AppId::new("magicpaper").is_ok());
    assert!(AppId::new("ko-reader2").is_ok());
    assert!(AppId::new("KOReader").is_err());
    assert!(AppId::new("../reader").is_err());
    assert!(AppId::new("-reader").is_err());
}

#[test]
fn reserved_environment_is_rejected() {
    let mut env = BTreeMap::new();
    env.insert("REMAGIC_SOCKET".into(), "/tmp/fake".into());
    let app = legacy_manifest(env);
    assert!(matches!(
        app.validate(),
        Err(ManifestError::UnsafeEnvironment(_))
    ));
}

#[test]
fn qtfb_display_backend_is_accepted() {
    let mut app = legacy_manifest(BTreeMap::new());
    app.exec = "/usr/bin/test".into();
    app.display = "qtfb".into();

    assert!(app.validate().is_ok());
    assert_eq!(app.runtime_profile(), RuntimeProfile::QtfbCompat);
}

#[test]
fn current_schema_v1_manifest_remains_readable() {
    let text = r#"
schema = 1
id = "magicpaper"
name = "MagicPaper"
exec = "/home/root/apps/remagic/libexec/magicpaper-qtfb"
working_dir = "/home/root/apps/riddle"
display = "qtfb"
park_strategy = "restart"
background_unit = "magicpaper-agent.service"

[environment]
REMAGIC_RUNTIME_SOCKET = "/run/remagic/runtime-app.sock"
LD_LIBRARY_PATH = "/home/root/apps/riddle:/usr/lib"
"#;
    let manifest: AppManifest = toml::from_str(text).unwrap();
    assert!(manifest.validate().is_ok());
    assert_eq!(manifest.schema, MANIFEST_SCHEMA_V1);
    assert_eq!(manifest.runtime_profile(), RuntimeProfile::QtfbCompat);
    assert!(matches!(
        manifest.effective_background_service(),
        Some(BackgroundService::Systemd { unit }) if unit == "magicpaper-agent.service"
    ));
}

#[test]
fn schema_v2_manifest_round_trips_with_full_runtime_contract() {
    let text = r#"
schema = 2
id = "koreader"
name = "KOReader"
version = "2026.03"
exec = "/opt/remagic/apps/koreader/current/bin/koreader-for-remagic"
working_dir = "/opt/remagic/apps/koreader/current"
display = "qtfb"
resident = true
capabilities = ["display:qtfb-v1", "input:touch-v1", "network:outbound-v1"]
supports_open_path = true
allowed_open_roots = ["/home/root/books"]

[readiness]
mode = "first_frame"
timeout_ms = 20000

[shutdown]
graceful_timeout_ms = 3500
term_timeout_ms = 4500
kill_timeout_ms = 5500

[background_service]
kind = "managed"
exec = "/opt/remagic/apps/koreader/current/bin/indexer"
args = ["--background"]
working_dir = "/opt/remagic/apps/koreader/current"
restart = "on_failure"

[data_schema]
version = 3
migrator = "/opt/remagic/apps/koreader/current/bin/migrate"
backup_paths = ["/home/root/.local/share/koreader"]
backup_timeout_ms = 120000
migration_timeout_ms = 120000

[runtime]
profile = "qtfb_compat"
required_libraries = ["libQt6Core.so.6", "libqsgepaper.so"]

[runtime.directories]
home = "/home/root"
config_home = "/home/root/.config/koreader"
data_home = "/home/root/.local/share/koreader"
state_home = "/home/root/.local/state/koreader"
cache_home = "/home/root/.cache/koreader"
runtime_dir = "/run/user/0/remagic/koreader"

[runtime.locale]
lang = "zh_CN.UTF-8"

[runtime.timezone]
name = "Asia/Shanghai"

[runtime.fonts]
directories = ["/home/root/.local/share/fonts"]

[runtime.certificates]
required = true
ca_bundle = "/etc/ssl/certs/ca-certificates.crt"

[runtime.network]
mode = "https_only"
allowed_hosts = ["api.example.com"]

[environment]
KOREADER_LANGUAGE = "zh_CN"
"#;
    let manifest: AppManifest = toml::from_str(text).unwrap();
    assert!(manifest.validate().is_ok());
    assert!(manifest.is_resident());
    assert_eq!(manifest.runtime_profile(), RuntimeProfile::QtfbCompat);
    let encoded = toml::to_string(&manifest).unwrap();
    let decoded: AppManifest = toml::from_str(&encoded).unwrap();
    assert_eq!(decoded, manifest);
}

#[test]
fn background_execution_defaults_to_continue_and_round_trips_freeze() {
    let mut app: AppManifest = toml::from_str(
        r#"
schema = 2
id = "reader"
name = "Reader"
exec = "/opt/reader/bin/reader"
working_dir = "/opt/reader"
display = "qtfb"
resident = true
capabilities = ["display:qtfb-v1", "lifecycle:v2"]

[runtime]
profile = "qtfb_compat"

[runtime.directories]
home = "/home/root"
config_home = "/home/root/.config/reader"
data_home = "/home/root/.local/share/reader"
state_home = "/home/root/.local/state/reader"
cache_home = "/home/root/.cache/reader"
runtime_dir = "/run/user/0/remagic/reader"
"#,
    )
    .unwrap();
    assert_eq!(
        app.runtime.background_execution,
        crate::BackgroundExecution::Continue
    );
    assert!(app.validate().is_ok());

    app.runtime.background_execution = crate::BackgroundExecution::Freeze;
    assert!(app.validate().is_ok());
    let encoded = toml::to_string(&app).unwrap();
    assert!(encoded.contains("background_execution = \"freeze\""));
    assert_eq!(toml::from_str::<AppManifest>(&encoded).unwrap(), app);
}

#[test]
fn freeze_requires_v2_residency_and_fenced_lifecycle() {
    let mut app = legacy_manifest(BTreeMap::new());
    app.runtime.background_execution = crate::BackgroundExecution::Freeze;
    assert!(matches!(
        app.validate(),
        Err(ManifestError::FreezeRequiresV2)
    ));

    app.schema = MANIFEST_SCHEMA_V2;
    app.runtime.profile = RuntimeProfile::Headless;
    app.runtime.directories = Some(crate::RuntimeDirectories {
        home: "/home/root".into(),
        config_home: "/home/root/.config/test".into(),
        data_home: "/home/root/.local/share/test".into(),
        state_home: "/home/root/.local/state/test".into(),
        cache_home: "/home/root/.cache/test".into(),
        runtime_dir: "/run/user/0/remagic/test".into(),
    });
    assert!(matches!(
        app.validate(),
        Err(ManifestError::FreezeRequiresResident)
    ));

    app.resident = true;
    assert!(matches!(
        app.validate(),
        Err(ManifestError::FreezeRequiresLifecycleV2)
    ));
    app.capabilities
        .push(crate::Capability::new("lifecycle:v2").unwrap());
    assert!(app.validate().is_ok());
}

#[test]
fn schema_v2_rejects_platform_environment_forgery() {
    for key in [
        "HOME",
        "XDG_DATA_HOME",
        "REMAGIC_APP_TOKEN",
        "LD_LIBRARY_PATH",
        "SSL_CERT_FILE",
    ] {
        let mut app = legacy_manifest(BTreeMap::from([(key.into(), "/tmp/forged".into())]));
        app.schema = MANIFEST_SCHEMA_V2;
        app.runtime.directories = Some(crate::RuntimeDirectories {
            home: "/home/root".into(),
            config_home: "/home/root/.config/test".into(),
            data_home: "/home/root/.local/share/test".into(),
            state_home: "/home/root/.local/state/test".into(),
            cache_home: "/home/root/.cache/test".into(),
            runtime_dir: "/run/user/0/remagic/test".into(),
        });
        assert!(matches!(
            app.validate(),
            Err(ManifestError::UnsafeEnvironment(actual)) if actual == key
        ));
    }

    let mut app = legacy_manifest(BTreeMap::from([("lowercase".into(), "value".into())]));
    app.schema = MANIFEST_SCHEMA_V2;
    app.runtime.directories = Some(crate::RuntimeDirectories {
        home: "/home/root".into(),
        config_home: "/home/root/.config/test".into(),
        data_home: "/home/root/.local/share/test".into(),
        state_home: "/home/root/.local/state/test".into(),
        cache_home: "/home/root/.cache/test".into(),
        runtime_dir: "/run/user/0/remagic/test".into(),
    });
    assert!(matches!(
        app.validate(),
        Err(ManifestError::UnsafeEnvironment(actual)) if actual == "lowercase"
    ));
}

#[test]
fn shutdown_deadline_ordering_is_checked_at_boundaries() {
    let mut app = legacy_manifest(BTreeMap::new());
    for (graceful, term, kill, valid) in [
        (100, 100, 100, true),
        (99, 100, 100, false),
        (101, 100, 102, false),
        (100, 102, 101, false),
        (100, 100, MAX_SHUTDOWN_KILL_TIMEOUT_MS, true),
        (100, 100, MAX_SHUTDOWN_KILL_TIMEOUT_MS + 1, false),
    ] {
        app.shutdown = ShutdownPolicy {
            graceful_timeout_ms: graceful,
            term_timeout_ms: term,
            kill_timeout_ms: kill,
        };
        assert_eq!(app.validate().is_ok(), valid, "{graceful}/{term}/{kill}");
    }
}

#[test]
fn schema_v2_binds_display_profile_and_capability() {
    let mut app = legacy_manifest(BTreeMap::new());
    app.schema = MANIFEST_SCHEMA_V2;
    app.display = "qtfb".into();
    app.runtime.profile = RuntimeProfile::QtfbCompat;
    app.runtime.directories = Some(crate::RuntimeDirectories {
        home: "/home/root".into(),
        config_home: "/home/root/.config/test".into(),
        data_home: "/home/root/.local/share/test".into(),
        state_home: "/home/root/.local/state/test".into(),
        cache_home: "/home/root/.cache/test".into(),
        runtime_dir: "/run/remagic/apps/test".into(),
    });
    assert!(matches!(
        app.validate(),
        Err(ManifestError::MissingRuntimeCapability {
            capability: "display:qtfb-v1",
            ..
        })
    ));
    app.capabilities
        .push(Capability::new("display:qtfb-v1").unwrap());
    assert!(app.validate().is_ok());
    app.display = "none".into();
    assert!(matches!(
        app.validate(),
        Err(ManifestError::RuntimeDisplayMismatch { .. })
    ));
}

#[test]
fn schema_v2_network_policy_matches_declared_capability() {
    let mut app = legacy_manifest(BTreeMap::new());
    app.schema = MANIFEST_SCHEMA_V2;
    app.display = "none".into();
    app.runtime.profile = RuntimeProfile::Headless;
    app.runtime.directories = Some(crate::RuntimeDirectories {
        home: "/home/root".into(),
        config_home: "/home/root/.config/test".into(),
        data_home: "/home/root/.local/share/test".into(),
        state_home: "/home/root/.local/state/test".into(),
        cache_home: "/home/root/.cache/test".into(),
        runtime_dir: "/run/remagic/apps/test".into(),
    });
    app.runtime.network.mode = NetworkMode::Outbound;
    assert!(matches!(
        app.validate(),
        Err(ManifestError::NetworkCapabilityMismatch { .. })
    ));
    app.capabilities
        .push(Capability::new("network:outbound-v1").unwrap());
    assert!(app.validate().is_ok());
}

#[test]
fn data_schema_rejects_ancestor_and_descendant_backup_paths() {
    let mut app = legacy_manifest(BTreeMap::new());
    app.schema = MANIFEST_SCHEMA_V2;
    app.runtime.profile = RuntimeProfile::Headless;
    app.runtime.directories = Some(crate::RuntimeDirectories {
        home: "/home/root".into(),
        config_home: "/home/root/.config/test".into(),
        data_home: "/home/root/.local/share/test".into(),
        state_home: "/home/root/.local/state/test".into(),
        cache_home: "/home/root/.cache/test".into(),
        runtime_dir: "/run/remagic/apps/test".into(),
    });
    app.data_schema = Some(DataSchema {
        version: 1,
        migrator: None,
        backup_paths: vec![
            "/home/root/.local/share/reader".into(),
            "/home/root/.local/share/reader/settings".into(),
        ],
        backup_timeout_ms: 1_000,
        migration_timeout_ms: 1_000,
    });
    assert!(matches!(
        app.validate(),
        Err(ManifestError::OverlappingBackupPaths(left, right))
            if left.as_path() == std::path::Path::new("/home/root/.local/share/reader")
                && right.as_path()
                    == std::path::Path::new("/home/root/.local/share/reader/settings")
    ));
}

#[test]
fn data_schema_is_not_executable_from_a_legacy_manifest() {
    let mut app = legacy_manifest(BTreeMap::new());
    app.data_schema = Some(DataSchema {
        version: 1,
        migrator: None,
        backup_paths: Vec::new(),
        backup_timeout_ms: 1_000,
        migration_timeout_ms: 1_000,
    });
    assert!(matches!(
        app.validate(),
        Err(ManifestError::DataSchemaRequiresV2)
    ));
}

#[test]
fn startup_timeout_preserves_backup_migration_and_readiness_budgets() {
    let mut app = legacy_manifest(BTreeMap::new());
    app.readiness.timeout_ms = 20_000;
    assert_eq!(app.startup_timeout_ms(), 20_000);
    app.data_schema = Some(DataSchema {
        version: 1,
        migrator: None,
        backup_paths: Vec::new(),
        backup_timeout_ms: 120_000,
        migration_timeout_ms: 120_000,
    });
    assert_eq!(app.startup_timeout_ms(), 270_000);
    app.display = "qtfb".into();
    assert_eq!(app.startup_timeout_ms(), 290_000);
}
