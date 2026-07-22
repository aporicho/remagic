use super::*;
use remagic_core::runtime::NetworkEnforcement;
use remagic_core::{qtfb_key_for_app, AppId, AppManifest, Capability};
use serde_json::Value;
use std::env;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

fn test_root(label: &str) -> PathBuf {
    let root = env::temp_dir().join(format!(
        "remagic-runner-{label}-{}-{}",
        std::process::id(),
        NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn executable(root: &Path) -> PathBuf {
    let executable = root.join("application");
    fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = fs::metadata(&executable).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&executable, permissions).unwrap();
    executable
}

fn platform(library_dir: &Path) -> PlatformRuntime {
    let root = library_dir.parent().unwrap();
    let qtfb_socket = root.join("qtfb.sock");
    let _ = fs::remove_file(&qtfb_socket);
    drop(UnixListener::bind(&qtfb_socket).unwrap());
    let zoneinfo_root = root.join("zoneinfo");
    let timezone = zoneinfo_root.join("Asia/Shanghai");
    fs::create_dir_all(timezone.parent().unwrap()).unwrap();
    fs::write(&timezone, b"test timezone").unwrap();
    PlatformRuntime {
        capabilities: DEFAULT_CAPABILITIES
            .iter()
            .map(|value| Capability::new((*value).to_owned()).unwrap())
            .collect(),
        qtfb_socket,
        path: DEFAULT_PATH.into(),
        library_search_dirs: vec![library_dir.to_path_buf()],
        zoneinfo_root,
        home_root: root.join("home"),
        runtime_root: root.join("runtime"),
        network_enforcement: NetworkEnforcement::MetadataOnly,
    }
}

fn v2_manifest(root: &Path, extra: &str) -> AppManifest {
    let executable = executable(root);
    let working_dir = root.join("work");
    fs::create_dir_all(&working_dir).unwrap();
    let text = format!(
        r#"
schema = 2
id = "koreader"
name = "KOReader for ReMagic"
exec = "{}"
working_dir = "{}"
display = "qtfb"
resident = true
capabilities = ["display:qtfb-v1", "input:touch-v1", "lifecycle:v2"]
{}

[runtime]
profile = "qtfb_compat"
required_libraries = ["librequired.so"]

[runtime.directories]
home = "{}"
config_home = "{}"
data_home = "{}"
state_home = "{}"
cache_home = "{}"
runtime_dir = "{}"

[runtime.locale]
lang = "C.UTF-8"

[runtime.timezone]
name = "Asia/Shanghai"

[runtime.network]
mode = "deny"
"#,
        executable.display(),
        working_dir.display(),
        extra,
        root.join("home").display(),
        root.join("home/config").display(),
        root.join("home/data").display(),
        root.join("home/state").display(),
        root.join("home/cache").display(),
        root.join("runtime/koreader").display(),
    );
    toml::from_str(&text).unwrap()
}

#[test]
fn schema_v2_builds_platform_owned_environment_and_creates_directories() {
    let root = test_root("v2");
    let library_dir = root.join("lib");
    fs::create_dir_all(&library_dir).unwrap();
    fs::write(library_dir.join("librequired.so"), b"test").unwrap();
    let manifest = v2_manifest(&root, "");
    let descriptor = LaunchDescriptor {
        generation: Some(91),
        foreground_epoch: Some(12),
        lease_id: Some(44),
        qtfb_key: Some(qtfb_key_for_app(&manifest.id)),
        ..LaunchDescriptor::default()
    };

    let plan = prepare_execution(&manifest, &descriptor, &platform(&library_dir)).unwrap();
    assert!(plan.clear_inherited_environment);
    assert_eq!(plan.generation, Some(91));
    assert_eq!(plan.variables["REMAGIC_APP_GENERATION"], "91");
    assert_eq!(plan.variables["REMAGIC_FOREGROUND_EPOCH"], "12");
    assert_eq!(plan.variables["REMAGIC_DISPLAY_LEASE_ID"], "44");
    assert_eq!(
        plan.variables["REMAGIC_NETWORK_POLICY_ENFORCEMENT"],
        "metadata_only"
    );
    assert_eq!(plan.variables["REMAGIC_NETWORK_ISOLATED"], "0");
    let token: Value = serde_json::from_str(&plan.variables["REMAGIC_APP_TOKEN"]).unwrap();
    assert_eq!(token["generation"], 91);
    assert_eq!(token["foreground_epoch"], 12);
    assert_eq!(token["lease_id"], 44);
    assert_eq!(
        plan.variables["QTFB_KEY"],
        qtfb_key_for_app(&manifest.id).to_string()
    );
    assert_eq!(
        plan.variables["REMAGIC_QTFB_SOCKET"],
        root.join("qtfb.sock").display().to_string()
    );
    assert_eq!(
        plan.variables["HOME"],
        root.join("home").display().to_string()
    );
    let environment = plan.launch_environment.unwrap();
    environment.validate().unwrap();
    assert_eq!(environment.resolved_libraries.len(), 1);
    for directory in [
        "home",
        "home/config",
        "home/data",
        "home/state",
        "home/cache",
        "runtime/koreader",
    ] {
        assert!(root.join(directory).is_dir(), "missing {directory}");
    }
    for directory in [
        "home/config",
        "home/data",
        "home/state",
        "home/cache",
        "runtime/koreader",
    ] {
        assert_eq!(
            fs::metadata(root.join(directory))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "unsafe mode for {directory}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn schema_v2_rejects_missing_capability_library_and_reserved_override() {
    let root = test_root("reject");
    let library_dir = root.join("lib");
    fs::create_dir_all(&library_dir).unwrap();
    let manifest = v2_manifest(&root, "");
    let descriptor = LaunchDescriptor {
        generation: Some(1),
        foreground_epoch: Some(2),
        lease_id: Some(3),
        qtfb_key: Some(qtfb_key_for_app(&manifest.id)),
        ..LaunchDescriptor::default()
    };
    let mut missing_capability = platform(&library_dir);
    missing_capability
        .capabilities
        .remove(&Capability::new("lifecycle:v2").unwrap());
    assert!(matches!(
        prepare_execution(&manifest, &descriptor, &missing_capability),
        Err(ExecutorError::MissingCapabilities(_))
    ));

    assert!(matches!(
        prepare_execution(&manifest, &descriptor, &platform(&library_dir)),
        Err(ExecutorError::MissingLibrary(_))
    ));

    fs::write(library_dir.join("librequired.so"), b"test").unwrap();
    let reserved = v2_manifest(&root, "[environment]\nQTFB_KEY = \"7\"");
    assert!(matches!(
        prepare_execution(&reserved, &descriptor, &platform(&library_dir)),
        Err(ExecutorError::ReservedEnvironment(key)) if key == "QTFB_KEY"
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn schema_v2_rejects_missing_generation_and_wrong_surface_key() {
    let root = test_root("token");
    let library_dir = root.join("lib");
    fs::create_dir_all(&library_dir).unwrap();
    fs::write(library_dir.join("librequired.so"), b"test").unwrap();
    let manifest = v2_manifest(&root, "");
    assert!(matches!(
        prepare_execution(
            &manifest,
            &LaunchDescriptor {
                foreground_epoch: Some(2),
                lease_id: Some(3),
                qtfb_key: Some(qtfb_key_for_app(&manifest.id)),
                ..LaunchDescriptor::default()
            },
            &platform(&library_dir)
        ),
        Err(ExecutorError::MissingGeneration)
    ));
    assert!(matches!(
        prepare_execution(
            &manifest,
            &LaunchDescriptor {
                generation: Some(1),
                lease_id: Some(3),
                qtfb_key: Some(qtfb_key_for_app(&manifest.id)),
                ..LaunchDescriptor::default()
            },
            &platform(&library_dir)
        ),
        Err(ExecutorError::MissingForegroundEpoch)
    ));
    assert!(matches!(
        prepare_execution(
            &manifest,
            &LaunchDescriptor {
                generation: Some(1),
                foreground_epoch: Some(2),
                qtfb_key: Some(qtfb_key_for_app(&manifest.id)),
                ..LaunchDescriptor::default()
            },
            &platform(&library_dir)
        ),
        Err(ExecutorError::MissingLease)
    ));
    assert!(matches!(
        prepare_execution(
            &manifest,
            &LaunchDescriptor {
                generation: Some(1),
                foreground_epoch: Some(2),
                lease_id: Some(3),
                qtfb_key: Some(qtfb_key_for_app(&manifest.id) + 1),
                ..LaunchDescriptor::default()
            },
            &platform(&library_dir)
        ),
        Err(ExecutorError::UnexpectedQtfbKey { .. })
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn schema_v1_preserves_legacy_environment_and_missing_launch_token() {
    let root = test_root("v1");
    let executable = executable(&root);
    let working_dir = root.join("work");
    fs::create_dir_all(&working_dir).unwrap();
    let manifest: AppManifest = toml::from_str(&format!(
        r#"
schema = 1
id = "magicpaper"
name = "MagicPaper"
exec = "{}"
working_dir = "{}"
display = "qtfb"

[environment]
HOME = "/home/root"
LD_LIBRARY_PATH = "/legacy/lib"
MAGICPAPER_SYSTEMD_MANAGED = "1"
"#,
        executable.display(),
        working_dir.display(),
    ))
    .unwrap();
    let plan = prepare_execution(
        &manifest,
        &LaunchDescriptor::default(),
        &platform(&root.join("lib")),
    )
    .unwrap();
    assert!(!plan.clear_inherited_environment);
    assert!(plan.launch_environment.is_none());
    assert_eq!(plan.generation, None);
    assert_eq!(plan.variables["HOME"], "/home/root");
    assert_eq!(plan.variables["LD_LIBRARY_PATH"], "/legacy/lib");
    assert_eq!(plan.variables["MAGICPAPER_SYSTEMD_MANAGED"], "1");
    assert!(!plan.variables.contains_key("XDG_DATA_HOME"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn schema_v2_preflight_requires_declared_fonts_certificates_timezone_and_qtfb_socket() {
    let root = test_root("resources");
    let library_dir = root.join("lib");
    fs::create_dir_all(&library_dir).unwrap();
    fs::write(library_dir.join("librequired.so"), b"test").unwrap();
    let descriptor = LaunchDescriptor {
        generation: Some(1),
        foreground_epoch: Some(2),
        lease_id: Some(3),
        qtfb_key: Some(qtfb_key_for_app(&AppId::new("koreader").unwrap())),
        ..LaunchDescriptor::default()
    };

    let mut missing_font = v2_manifest(&root, "");
    missing_font.runtime.fonts.directories = vec![root.join("missing-fonts")];
    assert!(matches!(
        prepare_execution(&missing_font, &descriptor, &platform(&library_dir)),
        Err(ExecutorError::RuntimeResource("font directory", _, _))
    ));

    let mut missing_ca = v2_manifest(&root, "");
    missing_ca.runtime.certificates.required = true;
    missing_ca.runtime.certificates.ca_bundle = Some(root.join("missing-ca.pem"));
    assert!(matches!(
        prepare_execution(&missing_ca, &descriptor, &platform(&library_dir)),
        Err(ExecutorError::RuntimeResource("CA bundle", _, _))
    ));

    let platform_without_timezone = platform(&library_dir);
    fs::remove_file(
        platform_without_timezone
            .zoneinfo_root
            .join("Asia/Shanghai"),
    )
    .unwrap();
    assert!(matches!(
        prepare_execution(
            &v2_manifest(&root, ""),
            &descriptor,
            &platform_without_timezone
        ),
        Err(ExecutorError::RuntimeResource("timezone data", _, _))
    ));

    let platform_without_socket = platform(&library_dir);
    fs::remove_file(&platform_without_socket.qtfb_socket).unwrap();
    assert!(matches!(
        prepare_execution(
            &v2_manifest(&root, ""),
            &descriptor,
            &platform_without_socket
        ),
        Err(ExecutorError::QtfbSocket(_, _))
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn schema_v2_runtime_directories_reject_symlinks_and_unsafe_existing_modes() {
    let root = test_root("directories");
    let library_dir = root.join("lib");
    fs::create_dir_all(&library_dir).unwrap();
    fs::write(library_dir.join("librequired.so"), b"test").unwrap();
    let manifest = v2_manifest(&root, "");
    let descriptor = LaunchDescriptor {
        generation: Some(1),
        foreground_epoch: Some(2),
        lease_id: Some(3),
        qtfb_key: Some(qtfb_key_for_app(&manifest.id)),
        ..LaunchDescriptor::default()
    };
    let mut wrong_home = manifest.clone();
    let alternate_home = root.join("alternate-home");
    let directories = wrong_home.runtime.directories.as_mut().unwrap();
    directories.home = alternate_home.clone();
    directories.config_home = alternate_home.join("config");
    directories.data_home = alternate_home.join("data");
    directories.state_home = alternate_home.join("state");
    directories.cache_home = alternate_home.join("cache");
    assert!(matches!(
        prepare_execution(&wrong_home, &descriptor, &platform(&library_dir)),
        Err(ExecutorError::UnexpectedHomeRoot { .. })
    ));

    let mut wrong_runtime = manifest.clone();
    wrong_runtime
        .runtime
        .directories
        .as_mut()
        .unwrap()
        .runtime_dir = root.join("other-runtime/koreader");
    assert!(matches!(
        prepare_execution(&wrong_runtime, &descriptor, &platform(&library_dir)),
        Err(ExecutorError::UnexpectedRuntimeDirectory { .. })
    ));

    fs::create_dir_all(root.join("home")).unwrap();
    let outside = root.join("outside");
    fs::create_dir_all(&outside).unwrap();
    symlink(&outside, root.join("home/config")).unwrap();
    assert!(matches!(
        prepare_execution(&manifest, &descriptor, &platform(&library_dir)),
        Err(ExecutorError::DirectorySymlink(_))
    ));

    fs::remove_file(root.join("home/config")).unwrap();
    fs::create_dir_all(root.join("home/data")).unwrap();
    fs::set_permissions(root.join("home/data"), fs::Permissions::from_mode(0o777)).unwrap();
    assert!(matches!(
        prepare_execution(&manifest, &descriptor, &platform(&library_dir)),
        Err(ExecutorError::UnsafeDirectoryMode(path, 0o777))
            if path == root.join("home/data")
    ));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn schema_v2_refuses_to_pretend_required_network_isolation_exists() {
    let root = test_root("network");
    let library_dir = root.join("lib");
    fs::create_dir_all(&library_dir).unwrap();
    fs::write(library_dir.join("librequired.so"), b"test").unwrap();
    let mut manifest = v2_manifest(&root, "");
    manifest.runtime.network.require_enforcement = true;
    let descriptor = LaunchDescriptor {
        generation: Some(1),
        foreground_epoch: Some(2),
        lease_id: Some(3),
        qtfb_key: Some(qtfb_key_for_app(&manifest.id)),
        ..LaunchDescriptor::default()
    };
    assert!(matches!(
        prepare_execution(&manifest, &descriptor, &platform(&library_dir)),
        Err(ExecutorError::Policy(message))
            if message.contains("only policy metadata is available")
    ));
    fs::remove_dir_all(root).unwrap();
}
