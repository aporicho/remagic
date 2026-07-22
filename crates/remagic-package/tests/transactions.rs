use flate2::write::GzEncoder;
use flate2::Compression;
use remagic_core::{AppId, DeviceProduct, DeviceProfile};
use remagic_package::{PackageError, PackageManager, PackagePaths};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tar::Builder;
use tempfile::TempDir;

struct Fixture {
    _temporary: TempDir,
    manager: PackageManager,
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().to_path_buf();
        let apps_root = root.join("apps");
        let manager = PackageManager::new(PackagePaths {
            staging_root: apps_root.join(".staging"),
            apps_root,
            manifest_root: root.join("manifests"),
            state_root: root.join("state"),
            books_root: root.join("books"),
        });
        Self {
            _temporary: temporary,
            manager,
            root,
        }
    }

    fn bundle(&self, version: &str, kind: &str, devices: &[&str]) -> PathBuf {
        let source = self.root.join(format!("source-{version}-{kind}"));
        fs::create_dir_all(source.join("payload/bin")).unwrap();
        let executable = source.join("payload/bin/demo");
        fs::write(&executable, format!("demo-{version}\n")).unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let supported = devices
            .iter()
            .map(|device| format!("\"{device}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let manifest = format!(
            r#"schema = 1
id = "demo"
name = "Demo"
kind = "{kind}"
version = "{version}"
package = "demo-package"
supported_devices = [{supported}]
supported_os = []
required_remagic_api = 2
uninstall_policy = "keep_data"
exec = "/home/root/apps/demo/current/payload/bin/demo"
working_dir = "/home/root/apps/demo/current"
display = "none"
"#
        );
        let manifest_path = source.join("manifest.toml");
        fs::write(&manifest_path, manifest).unwrap();
        fs::set_permissions(&manifest_path, fs::Permissions::from_mode(0o644)).unwrap();

        let mut files = [manifest_path, executable]
            .into_iter()
            .map(|path| inventory(&source, &path))
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
        let payload_sha256 = digest_records(
            files
                .iter()
                .filter(|entry| entry["path"].as_str().unwrap().starts_with("payload/")),
        );
        let mut content = Sha256::new();
        content.update(b"remagic-bundle-content-v1\0");
        for value in ["demo", "demo-package", version] {
            content.update(value.as_bytes());
            content.update(b"\0");
        }
        for entry in &files {
            content.update(record(entry));
        }
        let bundle = json!({
            "schema": 1,
            "app_id": "demo",
            "package": "demo-package",
            "version": version,
            "content_id": hex::encode(content.finalize()),
            "manifest_path": "manifest.toml",
            "payload_sha256": payload_sha256,
            "files": files,
        });
        fs::write(
            source.join("bundle.json"),
            serde_json::to_vec_pretty(&bundle).unwrap(),
        )
        .unwrap();
        fs::set_permissions(
            source.join("bundle.json"),
            fs::Permissions::from_mode(0o644),
        )
        .unwrap();

        let archive_path = self.root.join(format!("demo-{version}-{kind}.tar.gz"));
        let output = fs::File::create(&archive_path).unwrap();
        let encoder = GzEncoder::new(output, Compression::default());
        let mut archive = Builder::new(encoder);
        for path in ["bundle.json", "manifest.toml", "payload/bin/demo"] {
            archive
                .append_path_with_name(source.join(path), path)
                .unwrap();
        }
        archive.into_inner().unwrap().finish().unwrap();
        archive_path
    }
}

fn inventory(root: &Path, path: &Path) -> serde_json::Value {
    let metadata = fs::metadata(path).unwrap();
    json!({
        "path": path.strip_prefix(root).unwrap().to_string_lossy(),
        "sha256": hex::encode(Sha256::digest(fs::read(path).unwrap())),
        "size": metadata.len(),
        "mode": format!("{:04o}", metadata.permissions().mode() & 0o7777),
    })
}

fn record(entry: &serde_json::Value) -> Vec<u8> {
    let mode = u32::from_str_radix(entry["mode"].as_str().unwrap(), 8).unwrap();
    format!(
        "{}\0{:o}\0{}\0{}\n",
        entry["path"].as_str().unwrap(),
        mode,
        entry["size"].as_u64().unwrap(),
        entry["sha256"].as_str().unwrap()
    )
    .into_bytes()
}

fn digest_records<'a>(entries: impl Iterator<Item = &'a serde_json::Value>) -> String {
    let mut digest = Sha256::new();
    for entry in entries {
        digest.update(record(entry));
    }
    hex::encode(digest.finalize())
}

#[test]
fn install_upgrade_rollback_and_uninstall_preserve_books() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.manager.paths().books_root.clone()).unwrap();
    fs::write(
        fixture.manager.paths().books_root.join("book.epub"),
        b"book",
    )
    .unwrap();
    let device = DeviceProfile::for_product(DeviceProduct::PaperProMove, "5.7.126");

    let first = fixture
        .manager
        .prepare(
            &fixture.bundle("1.0.0", "user", &["paper_pro_move"]),
            &device,
        )
        .unwrap();
    let first = fixture.manager.install(first).unwrap();
    let first_content = first.content_id.clone();
    assert_eq!(
        fs::read_link(fixture.root.join("apps/demo/current")).unwrap(),
        Path::new("releases").join(&first_content)
    );
    let installed_manifest = fs::read_to_string(fixture.root.join("manifests/demo.toml")).unwrap();
    assert!(installed_manifest.contains(&format!(
        "/home/root/apps/demo/releases/{first_content}/payload/bin/demo"
    )));
    assert!(!installed_manifest.contains("/home/root/apps/demo/current"));
    let executable = fixture
        .root
        .join("apps/demo/releases")
        .join(&first_content)
        .join("payload/bin/demo");
    assert_eq!(
        fs::metadata(executable).unwrap().permissions().mode() & 0o222,
        0
    );

    let second = fixture
        .manager
        .prepare(
            &fixture.bundle("2.0.0", "user", &["paper_pro_move"]),
            &device,
        )
        .unwrap();
    let second = fixture.manager.install(second).unwrap();
    assert_eq!(
        second.previous_content_id.as_deref(),
        Some(first_content.as_str())
    );
    let rolled_back = fixture
        .manager
        .rollback(&AppId::new("demo").unwrap(), None, &device)
        .unwrap();
    assert_eq!(rolled_back.content_id, first_content);
    let rolled_back_manifest =
        fs::read_to_string(fixture.root.join("manifests/demo.toml")).unwrap();
    assert!(rolled_back_manifest.contains(&format!(
        "/home/root/apps/demo/releases/{}/payload/bin/demo",
        rolled_back.content_id
    )));
    assert!(!rolled_back_manifest.contains("/home/root/apps/demo/current"));

    fixture
        .manager
        .uninstall(&AppId::new("demo").unwrap(), false)
        .unwrap();
    assert!(!fixture.root.join("apps/demo").exists());
    assert!(fixture
        .manager
        .paths()
        .books_root
        .join("book.epub")
        .exists());
}

#[test]
fn reinstalling_the_current_content_is_an_idempotent_success() {
    let fixture = Fixture::new();
    let device = DeviceProfile::for_product(DeviceProduct::PaperPro, "3.27.3.0");
    let bundle = fixture.bundle("1.0.0", "user", &["paper_pro"]);
    let first = fixture
        .manager
        .install(fixture.manager.prepare(&bundle, &device).unwrap())
        .unwrap();
    let second = fixture
        .manager
        .install(fixture.manager.prepare(&bundle, &device).unwrap())
        .unwrap();
    assert_eq!(first.content_id, second.content_id);
    assert_eq!(first.previous_content_id, second.previous_content_id);
    assert_eq!(
        fs::read_link(fixture.root.join("apps/demo/current")).unwrap(),
        PathBuf::from("releases").join(first.content_id)
    );
    let installed_manifest = fs::read_to_string(fixture.root.join("manifests/demo.toml")).unwrap();
    assert!(!installed_manifest.contains("/home/root/apps/demo/current"));
}

#[test]
fn incompatible_device_is_rejected_before_publication() {
    let fixture = Fixture::new();
    let device = DeviceProfile::for_product(DeviceProduct::PaperProMove, "5.7.126");
    let error = fixture
        .manager
        .prepare(&fixture.bundle("1.0.0", "user", &["paper_pro"]), &device)
        .unwrap_err();
    assert!(matches!(error, PackageError::Compatibility(_)));
    assert!(!fixture.root.join("apps/demo").exists());
}

#[test]
fn system_application_cannot_be_uninstalled() {
    let fixture = Fixture::new();
    let device = DeviceProfile::for_product(DeviceProduct::PaperPro, "5.7.126");
    let package = fixture
        .manager
        .prepare(&fixture.bundle("1.0.0", "system", &["paper_pro"]), &device)
        .unwrap();
    fixture.manager.install(package).unwrap();
    let error = fixture
        .manager
        .uninstall(&AppId::new("demo").unwrap(), false)
        .unwrap_err();
    assert!(matches!(error, PackageError::SystemApp(_)));
    assert!(fixture.root.join("apps/demo/current").exists());
}

#[test]
fn restrictive_umask_child() {
    if std::env::var_os("REMAGIC_TEST_RESTRICTIVE_UMASK_CHILD").is_none() {
        return;
    }
    let fixture = Fixture::new();
    let device = DeviceProfile::for_product(DeviceProduct::PaperProMove, "3.27.3.0");
    let bundle = fixture.bundle("1.0.0", "user", &["paper_pro_move"]);
    let prepared = fixture.manager.prepare(&bundle, &device).unwrap();
    fixture.manager.install(prepared).unwrap();

    let manifest_mode = fs::metadata(fixture.root.join("manifests/demo.toml"))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    let state_mode = fs::metadata(fixture.root.join("state/demo.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o7777;
    assert_eq!(manifest_mode, 0o644);
    assert_eq!(state_mode, 0o600);
}

#[test]
fn package_install_is_independent_of_the_daemon_umask() {
    let current_test = std::env::current_exe().unwrap();
    let status = Command::new("sh")
        .args([
            "-c",
            "umask 077; exec \"$1\" --exact restrictive_umask_child --nocapture",
            "remagic-umask-test",
        ])
        .arg(current_test)
        .env("REMAGIC_TEST_RESTRICTIVE_UMASK_CHILD", "1")
        .status()
        .unwrap();
    assert!(status.success());
}
