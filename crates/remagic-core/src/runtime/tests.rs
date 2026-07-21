use super::*;
use std::collections::{BTreeMap, BTreeSet};

fn directories() -> RuntimeDirectories {
    RuntimeDirectories {
        home: "/home/root".into(),
        config_home: "/home/root/.config/test".into(),
        data_home: "/home/root/.local/share/test".into(),
        state_home: "/home/root/.local/state/test".into(),
        cache_home: "/home/root/.cache/test".into(),
        runtime_dir: "/run/user/0/remagic/test".into(),
    }
}

fn launch_environment() -> LaunchEnvironment {
    let directories = directories();
    let variables = BTreeMap::from([
        ("HOME".into(), directories.home.display().to_string()),
        (
            "XDG_CONFIG_HOME".into(),
            directories.config_home.display().to_string(),
        ),
        (
            "XDG_DATA_HOME".into(),
            directories.data_home.display().to_string(),
        ),
        (
            "XDG_STATE_HOME".into(),
            directories.state_home.display().to_string(),
        ),
        (
            "XDG_CACHE_HOME".into(),
            directories.cache_home.display().to_string(),
        ),
        (
            "XDG_RUNTIME_DIR".into(),
            directories.runtime_dir.display().to_string(),
        ),
        ("LANG".into(), "zh_CN.UTF-8".into()),
        ("TZ".into(), "Asia/Shanghai".into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("REMAGIC_APP_ID".into(), "test".into()),
        ("REMAGIC_RUNTIME_PROFILE".into(), "native_v2".into()),
        ("REMAGIC_NETWORK_POLICY_MODE".into(), "deny".into()),
        (
            "REMAGIC_NETWORK_POLICY_ENFORCEMENT".into(),
            "metadata_only".into(),
        ),
        ("REMAGIC_NETWORK_ISOLATED".into(), "0".into()),
        ("REMAGIC_NETWORK_ALLOWED_HOSTS".into(), String::new()),
    ]);
    LaunchEnvironment {
        app_id: AppId::new("test").unwrap(),
        profile: RuntimeProfile::NativeV2,
        directories,
        variables,
        resolved_libraries: vec!["/usr/lib/libqsgepaper.so".into()],
        platform_capabilities: BTreeSet::from([
            Capability::new("display:surface-v2").unwrap(),
            Capability::new("ink:direct-v1").unwrap(),
        ]),
        locale: LocalePolicy {
            lang: "zh_CN.UTF-8".into(),
            lc_all: None,
        },
        timezone: TimezonePolicy {
            name: "Asia/Shanghai".into(),
        },
        fonts: FontPolicy::default(),
        certificates: CertificatePolicy::default(),
        network: NetworkPolicy::default(),
    }
}

#[test]
fn capability_syntax_boundaries_are_stable() {
    for valid in ["a", "display:qtfb-v1", "ink:direct-v1", "feature_2.test"] {
        assert!(Capability::new(valid).is_ok(), "{valid}");
    }
    for invalid in ["", "UPPER", ":leading", "trailing-", "white space", "a/b"] {
        assert!(Capability::new(invalid).is_err(), "{invalid}");
    }
    assert!(Capability::new("a".repeat(96)).is_ok());
    assert!(Capability::new("a".repeat(97)).is_err());
}

#[test]
fn qtfb_keys_are_stable_positive_and_do_not_use_home_reservation() {
    let vectors = [
        ("magicpaper", 1_599_673_631),
        ("koreader", 1_340_486_485),
        ("remagic-home", 1_132_954_932),
    ];
    for (name, expected) in vectors {
        let app_id = AppId::new(name).unwrap();
        let actual = qtfb_key_for_app(&app_id);
        assert_eq!(actual, expected, "stable vector for {name}");
        assert!(actual > 0);
        assert_ne!(actual, REMAGIC_HOME_QTFB_KEY);
    }
}

#[test]
fn launch_environment_requires_platform_owned_xdg_values() {
    let mut environment = launch_environment();
    assert!(environment.validate().is_ok());
    environment
        .variables
        .insert("XDG_DATA_HOME".into(), "/tmp/forged".into());
    assert!(matches!(
        environment.validate(),
        Err(RuntimeValidationError::MismatchedLaunchVariable(
            "XDG_DATA_HOME",
            _,
            _
        ))
    ));
}

#[test]
fn preflight_compatibility_is_derived_from_failures_and_missing_items() {
    let environment = launch_environment();
    let report = PreflightReport {
        app_id: environment.app_id.clone(),
        profile: environment.profile,
        compatible: true,
        checks: vec![PreflightCheck {
            id: "display-owner".into(),
            status: PreflightStatus::Passed,
            message: "available".into(),
        }],
        missing_capabilities: BTreeSet::new(),
        missing_libraries: Vec::new(),
        launch_environment: Some(environment),
    };
    assert!(report.validate().is_ok());

    let mut invalid = report.clone();
    invalid.missing_libraries.push("libmissing.so".into());
    assert_eq!(
        invalid.validate(),
        Err(RuntimeValidationError::IncoherentPreflight)
    );
}

#[test]
fn runtime_policy_rejects_escape_paths_and_network_policy_conflicts() {
    let mut requirements = RuntimeRequirements {
        directories: Some(directories()),
        ..RuntimeRequirements::default()
    };
    assert!(requirements.validate(true).is_ok());
    requirements.directories.as_mut().unwrap().cache_home = "../cache".into();
    assert!(matches!(
        requirements.validate(true),
        Err(RuntimeValidationError::UnsafePath("cache_home", _))
    ));

    requirements.directories = Some(directories());
    requirements
        .network
        .allowed_hosts
        .insert("api.example.com".into());
    assert_eq!(
        requirements.validate(true),
        Err(RuntimeValidationError::HostsWithDeniedNetwork)
    );
}

#[test]
fn runtime_types_round_trip() {
    let environment = launch_environment();
    let encoded = serde_json::to_vec(&environment).unwrap();
    let decoded: LaunchEnvironment = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded, environment);
}

#[test]
fn resolver_builds_complete_platform_environment_and_rejects_spoofing() {
    let requirements = RuntimeRequirements {
        profile: RuntimeProfile::QtfbCompat,
        required_libraries: vec!["libqsgepaper.so".into()],
        directories: Some(directories()),
        locale: LocalePolicy {
            lang: "zh_CN.UTF-8".into(),
            lc_all: Some("zh_CN.UTF-8".into()),
        },
        timezone: TimezonePolicy {
            name: "Asia/Shanghai".into(),
        },
        fonts: FontPolicy {
            directories: vec!["/home/root/.local/share/fonts".into()],
            fontconfig_file: Some("/home/root/.config/fontconfig/fonts.conf".into()),
        },
        certificates: CertificatePolicy {
            required: true,
            ca_bundle: Some("/etc/ssl/certs/ca-certificates.crt".into()),
            ca_directory: None,
        },
        network: NetworkPolicy {
            mode: NetworkMode::HttpsOnly,
            allowed_hosts: BTreeSet::from(["api.example.com".into()]),
            require_enforcement: false,
        },
    };
    let app_id = AppId::new("koreader").unwrap();
    let environment = LaunchEnvironment::resolve(
        app_id.clone(),
        &requirements,
        &BTreeMap::from([("KOREADER_LANGUAGE".into(), "zh_CN".into())]),
        vec!["/usr/lib/libqsgepaper.so".into()],
        BTreeSet::from([Capability::new("display:qtfb-v1").unwrap()]),
        "/usr/bin:/bin",
        NetworkEnforcement::MetadataOnly,
    )
    .unwrap();
    assert_eq!(environment.variables["REMAGIC_APP_ID"], "koreader");
    assert_eq!(
        environment.variables["QTFB_KEY"],
        qtfb_key_for_app(&app_id).to_string()
    );
    assert_eq!(environment.variables["LD_LIBRARY_PATH"], "/usr/lib");
    assert_eq!(
        environment.variables["SSL_CERT_FILE"],
        "/etc/ssl/certs/ca-certificates.crt"
    );
    assert!(environment.validate().is_ok());

    let spoofed = LaunchEnvironment::resolve(
        app_id,
        &requirements,
        &BTreeMap::from([("HOME".into(), "/tmp/fake".into())]),
        vec!["/usr/lib/libqsgepaper.so".into()],
        BTreeSet::new(),
        "/usr/bin:/bin",
        NetworkEnforcement::MetadataOnly,
    );
    assert_eq!(
        spoofed,
        Err(RuntimeValidationError::ReservedApplicationEnvironment(
            "HOME".into()
        ))
    );
}

#[test]
fn network_policy_never_claims_isolation_without_enforcement() {
    let mut requirements = RuntimeRequirements {
        directories: Some(directories()),
        ..RuntimeRequirements::default()
    };
    let environment = LaunchEnvironment::resolve(
        AppId::new("test").unwrap(),
        &requirements,
        &BTreeMap::new(),
        Vec::new(),
        BTreeSet::new(),
        "/usr/bin:/bin",
        NetworkEnforcement::MetadataOnly,
    )
    .unwrap();
    assert_eq!(
        environment.variables["REMAGIC_NETWORK_POLICY_ENFORCEMENT"],
        "metadata_only"
    );
    assert_eq!(environment.variables["REMAGIC_NETWORK_ISOLATED"], "0");

    requirements.network.require_enforcement = true;
    assert_eq!(
        LaunchEnvironment::resolve(
            AppId::new("test").unwrap(),
            &requirements,
            &BTreeMap::new(),
            Vec::new(),
            BTreeSet::new(),
            "/usr/bin:/bin",
            NetworkEnforcement::MetadataOnly,
        ),
        Err(RuntimeValidationError::RequiredNetworkEnforcementUnavailable)
    );
}
