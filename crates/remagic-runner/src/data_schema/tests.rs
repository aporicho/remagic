use super::*;
use remagic_core::runtime::NetworkEnforcement;
use remagic_core::{AppId, DataSchema, LaunchEnvironment};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
static PROCESS_ENVIRONMENT: Mutex<()> = Mutex::new(());
// Process creation inherits descriptors until exec closes O_CLOEXEC handles.
// Serializing independent fixtures avoids a test-only fork window where a
// migrator from one case briefly retains another case's advisory lock.
static SCHEMA_FIXTURES: Mutex<()> = Mutex::new(());

struct Fixture {
    _serial: MutexGuard<'static, ()>,
    root: PathBuf,
    manifest: AppManifest,
    environment: LaunchEnvironment,
    state_root: PathBuf,
    source: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Self {
        let serial = SCHEMA_FIXTURES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let root = std::env::temp_dir().join(format!(
            "remagic-schema-{label}-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        let home = root.join("home");
        let work = root.join("work");
        let source = home.join("data");
        let state_home = home.join("state");
        for directory in [
            &home,
            &work,
            &source,
            &state_home,
            &home.join("config"),
            &home.join("cache"),
            &root.join("runtime"),
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        let text = format!(
            r#"
schema = 2
id = "test-app"
name = "Test App"
exec = "/bin/true"
working_dir = "{}"
display = "none"

[runtime]
profile = "headless"

[runtime.directories]
home = "{}"
config_home = "{}"
data_home = "{}"
state_home = "{}"
cache_home = "{}"
runtime_dir = "{}"

[runtime.network]
mode = "deny"
"#,
            work.display(),
            home.display(),
            home.join("config").display(),
            source.display(),
            state_home.display(),
            home.join("cache").display(),
            root.join("runtime").display(),
        );
        let mut manifest: AppManifest = toml::from_str(&text).unwrap();
        manifest.data_schema = Some(DataSchema {
            version: 1,
            migrator: None,
            backup_paths: vec![source.clone()],
            backup_timeout_ms: 1_000,
            migration_timeout_ms: 1_000,
        });
        manifest.validate().unwrap();
        let environment = resolve_environment(&manifest);
        let state_root = state_home.join(MANAGED_STATE_DIRECTORY);
        Self {
            _serial: serial,
            root,
            manifest,
            environment,
            state_root,
            source,
        }
    }

    fn schema(&self) -> &DataSchema {
        self.manifest.data_schema.as_ref().unwrap()
    }

    fn apply(&self) -> Result<(), DataSchemaError> {
        apply_at(
            &self.manifest,
            self.schema(),
            &self.environment,
            &self.state_root,
        )
    }

    fn set_version(&mut self, version: u32) {
        self.manifest.data_schema.as_mut().unwrap().version = version;
    }

    fn set_migrator(&mut self, path: PathBuf) {
        self.manifest.data_schema.as_mut().unwrap().migrator = Some(path);
    }

    fn refresh_environment(&mut self) {
        self.environment = resolve_environment(&self.manifest);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn resolve_environment(manifest: &AppManifest) -> LaunchEnvironment {
    LaunchEnvironment::resolve(
        manifest.id.clone(),
        &manifest.runtime,
        &manifest.environment,
        Vec::new(),
        BTreeSet::new(),
        "/usr/bin:/bin",
        NetworkEnforcement::MetadataOnly,
    )
    .unwrap()
}

fn write_script(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(name);
    fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    path
}

fn state_version(state_root: &Path) -> u64 {
    let state: Value =
        serde_json::from_slice(&fs::read(state_root.join("state.json")).unwrap()).unwrap();
    state["version"].as_u64().unwrap()
}

fn ready_fence(state_root: &Path) -> String {
    fs::read_to_string(state_root.join(remagic_core::SCHEMA_READY_FILE)).unwrap()
}

fn backup_directories(state_root: &Path) -> Vec<PathBuf> {
    let mut paths = fs::read_dir(state_root.join("backups"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            !path
                .file_name()
                .unwrap()
                .as_encoded_bytes()
                .starts_with(b".")
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths
}

fn publish_pending(
    fixture: &Fixture,
    snapshot: &backup::Snapshot,
    from_version: Option<u32>,
    to_version: u32,
    backup_paths: Vec<PathBuf>,
) {
    SchemaStateStore::open(&fixture.state_root, &fixture.manifest.id)
        .unwrap()
        .publish_pending(&PendingMigration {
            format: PENDING_FORMAT,
            app_id: fixture.manifest.id.clone(),
            from_version,
            to_version,
            backup: snapshot.name().to_owned(),
            backup_paths,
        })
        .unwrap();
}

#[test]
fn first_application_backs_up_files_and_symlinks_then_same_version_is_a_noop() {
    let fixture = Fixture::new("first");
    fs::write(fixture.source.join("book.txt"), b"original bytes").unwrap();
    fs::write(fixture.root.join("outside.txt"), b"must not be followed").unwrap();
    symlink("../../outside.txt", fixture.source.join("external-link")).unwrap();

    fixture.apply().unwrap();
    assert_eq!(state_version(&fixture.state_root), 1);
    assert_eq!(ready_fence(&fixture.state_root), "test-app:1\n");
    let backups = backup_directories(&fixture.state_root);
    assert_eq!(backups.len(), 1);
    let backed_up_source = backups[0].join("sources/0000");
    assert_eq!(
        fs::read(backed_up_source.join("book.txt")).unwrap(),
        b"original bytes"
    );
    let link_metadata = fs::symlink_metadata(backed_up_source.join("external-link")).unwrap();
    assert!(link_metadata.file_type().is_symlink());
    assert_eq!(
        fs::read_link(backed_up_source.join("external-link")).unwrap(),
        PathBuf::from("../../outside.txt")
    );

    fs::write(fixture.source.join("book.txt"), b"changed after commit").unwrap();
    fixture.apply().unwrap();
    assert_eq!(backup_directories(&fixture.state_root), backups);
    assert_eq!(
        fs::read(fixture.source.join("book.txt")).unwrap(),
        b"changed after commit"
    );
}

#[test]
fn persistent_ready_fence_tracks_only_the_applied_schema() {
    let mut fixture = Fixture::new("ready-fence");
    fixture.apply().unwrap();
    assert_eq!(ready_fence(&fixture.state_root), "test-app:1\n");

    fixture.set_version(2);
    fixture.apply().unwrap();
    assert_eq!(ready_fence(&fixture.state_root), "test-app:2\n");

    fixture.set_version(1);
    assert!(matches!(
        fixture.apply(),
        Err(DataSchemaError::Downgrade {
            applied: 2,
            requested: 1
        })
    ));
    assert_eq!(ready_fence(&fixture.state_root), "test-app:2\n");
}

#[test]
fn upgrade_uses_clean_resolved_environment_working_directory_and_runs_once() {
    let _environment_guard = PROCESS_ENVIRONMENT.lock().unwrap();
    let mut fixture = Fixture::new("upgrade");
    let data_file = fixture.source.join("book.txt");
    let observations = fixture.root.join("observations.txt");
    fs::write(&data_file, b"v1").unwrap();
    fixture.apply().unwrap();

    let inherited_key = format!("REMAGIC_TEST_SECRET_{}", std::process::id());
    std::env::set_var(&inherited_key, "must-not-leak");
    fixture.set_version(2);
    fixture.manifest.environment = BTreeMap::from([
        ("TEST_DATA_FILE".into(), data_file.display().to_string()),
        (
            "TEST_OBSERVATIONS".into(),
            observations.display().to_string(),
        ),
        ("TEST_DECLARED".into(), "declared-value".into()),
    ]);
    let script_body = format!(
        concat!(
            "printf '%s|%s|%s|%s|%s|%s\\n' ",
            "\"$REMAGIC_DATA_SCHEMA_FROM\" \"$REMAGIC_DATA_SCHEMA_TO\" ",
            "\"$PWD\" \"$TEST_DECLARED\" ",
            "\"${{{key}-unset}}\" \"$REMAGIC_DATA_SCHEMA_BACKUP\" ",
            ">> \"$TEST_OBSERVATIONS\"\n",
            "printf 'v2' > \"$TEST_DATA_FILE\""
        ),
        key = inherited_key
    );
    let script = write_script(&fixture.root, "migrate-success", &script_body);
    fixture.set_migrator(script);
    fixture.refresh_environment();

    fixture.apply().unwrap();
    std::env::remove_var(&inherited_key);
    assert_eq!(state_version(&fixture.state_root), 2);
    assert_eq!(fs::read(&data_file).unwrap(), b"v2");
    let lines = fs::read_to_string(&observations).unwrap();
    let fields: Vec<_> = lines.trim().split('|').collect();
    assert_eq!(fields[0], "1");
    assert_eq!(fields[1], "2");
    assert_eq!(fields[2], fixture.manifest.working_dir.to_str().unwrap());
    assert_eq!(fields[3], "declared-value");
    assert_eq!(fields[4], "unset");
    assert!(Path::new(fields[5]).is_dir());

    fixture.apply().unwrap();
    assert_eq!(
        fs::read_to_string(&observations).unwrap().lines().count(),
        1
    );
}

#[test]
fn failed_migration_restores_data_and_absent_paths_then_can_retry_safely() {
    let mut fixture = Fixture::new("failure");
    let data_file = fixture.source.join("book.txt");
    let initially_absent = fixture.root.join("initially-absent");
    fs::write(&data_file, b"v1").unwrap();
    fixture
        .manifest
        .data_schema
        .as_mut()
        .unwrap()
        .backup_paths
        .push(initially_absent.clone());
    fixture.apply().unwrap();

    fixture.set_version(2);
    fixture.manifest.environment = BTreeMap::from([
        ("TEST_DATA_FILE".into(), data_file.display().to_string()),
        (
            "TEST_ABSENT_PATH".into(),
            initially_absent.display().to_string(),
        ),
    ]);
    let failing = write_script(
        &fixture.root,
        "migrate-failure",
        "printf 'partial' > \"$TEST_DATA_FILE\"\nmkdir -p \"$TEST_ABSENT_PATH\"\nprintf 'new' > \"$TEST_ABSENT_PATH/file\"\nexit 7",
    );
    fixture.set_migrator(failing);
    fixture.refresh_environment();
    let failed = fixture.apply();
    assert!(
        matches!(failed, Err(DataSchemaError::MigratorFailed { .. })),
        "unexpected failed migration result: {failed:?}"
    );
    assert_eq!(state_version(&fixture.state_root), 1);
    assert_eq!(fs::read(&data_file).unwrap(), b"v1");
    assert!(!initially_absent.exists());

    let succeeding = write_script(
        &fixture.root,
        "migrate-retry",
        "test \"$(cat \"$TEST_DATA_FILE\")\" = 'post-failure-valid'\nprintf 'v2-after-retry' > \"$TEST_DATA_FILE\"",
    );
    fs::write(&data_file, b"post-failure-valid").unwrap();
    fixture.set_migrator(succeeding);
    fixture.apply().unwrap();
    assert_eq!(state_version(&fixture.state_root), 2);
    assert_eq!(fs::read(&data_file).unwrap(), b"v2-after-retry");
}

#[test]
fn published_backup_recovers_an_interrupted_migration_before_retry() {
    let mut fixture = Fixture::new("crash-retry");
    let data_file = fixture.source.join("book.txt");
    fs::write(&data_file, b"v1-baseline").unwrap();
    fixture.apply().unwrap();

    let backups = BackupStore::new(
        fixture.state_root.join("backups"),
        fixture.manifest.id.clone(),
    );
    let snapshot = backups
        .snapshot(Some(1), 2, std::slice::from_ref(&fixture.source))
        .unwrap();
    publish_pending(
        &fixture,
        &snapshot,
        Some(1),
        2,
        vec![fixture.source.clone()],
    );
    // This is the on-disk shape left when power is lost after a migrator has
    // written data but before state.json is atomically advanced.
    fs::write(&data_file, b"partial-from-crash").unwrap();

    fixture.set_version(2);
    fixture.manifest.environment =
        BTreeMap::from([("TEST_DATA_FILE".into(), data_file.display().to_string())]);
    let retry = write_script(
        &fixture.root,
        "migrate-after-crash",
        "test \"$(cat \"$TEST_DATA_FILE\")\" = 'v1-baseline'\nprintf 'v2' > \"$TEST_DATA_FILE\"",
    );
    fixture.set_migrator(retry);
    fixture.refresh_environment();
    fixture.apply().unwrap();
    assert_eq!(state_version(&fixture.state_root), 2);
    assert_eq!(fs::read(data_file).unwrap(), b"v2");
}

#[test]
fn interrupted_upgrade_is_restored_before_same_version_manifest_can_launch() {
    let fixture = Fixture::new("rollback-manifest");
    let data_file = fixture.source.join("book.txt");
    fs::write(&data_file, b"v1-baseline").unwrap();
    fs::set_permissions(&data_file, fs::Permissions::from_mode(0o640)).unwrap();
    fixture.apply().unwrap();

    let backups = BackupStore::new(
        fixture.state_root.join("backups"),
        fixture.manifest.id.clone(),
    );
    let snapshot = backups
        .snapshot(Some(1), 2, std::slice::from_ref(&fixture.source))
        .unwrap();
    publish_pending(
        &fixture,
        &snapshot,
        Some(1),
        2,
        vec![fixture.source.clone()],
    );
    fs::write(&data_file, b"partial-v2").unwrap();
    fs::set_permissions(&data_file, fs::Permissions::from_mode(0o600)).unwrap();

    // The installed manifest has been rolled back to v1. Recovery must still
    // happen before the same-version fast path is considered.
    fixture.apply().unwrap();
    assert_eq!(state_version(&fixture.state_root), 1);
    assert_eq!(fs::read(&data_file).unwrap(), b"v1-baseline");
    assert_eq!(
        fs::metadata(&data_file).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert!(!fixture.state_root.join("pending.json").exists());
    assert!(!fixture.state_root.join("backups/from-1-to-2").exists());
}

#[test]
fn recovery_uses_only_journaled_paths_then_resnapshots_changed_manifest_paths() {
    let mut fixture = Fixture::new("pending-binding");
    let original_file = fixture.source.join("book.txt");
    let replacement_source = fixture.root.join("replacement-data");
    fs::create_dir_all(&replacement_source).unwrap();
    let replacement_file = replacement_source.join("notes.txt");
    fs::write(&original_file, b"v1-original").unwrap();
    fs::write(&replacement_file, b"replacement-baseline").unwrap();
    fixture.apply().unwrap();

    let backups = BackupStore::new(
        fixture.state_root.join("backups"),
        fixture.manifest.id.clone(),
    );
    let pending = backups
        .snapshot(Some(1), 2, std::slice::from_ref(&fixture.source))
        .unwrap();
    publish_pending(&fixture, &pending, Some(1), 2, vec![fixture.source.clone()]);
    // This valid but unjournaled snapshot has the same source version. It must
    // not participate in recovery.
    backups
        .snapshot(Some(1), 3, std::slice::from_ref(&replacement_source))
        .unwrap();
    fs::write(&original_file, b"partial-v2").unwrap();
    fs::write(&replacement_file, b"new-live-value").unwrap();

    fixture.set_version(2);
    fixture.manifest.data_schema.as_mut().unwrap().backup_paths = vec![replacement_source.clone()];
    fixture.apply().unwrap();

    assert_eq!(fs::read(original_file).unwrap(), b"v1-original");
    assert_eq!(fs::read(replacement_file).unwrap(), b"new-live-value");
    assert_eq!(state_version(&fixture.state_root), 2);
    assert!(!fixture.state_root.join("pending.json").exists());
    assert!(fixture.state_root.join("backups/from-1-to-3").exists());
}

#[test]
fn committed_state_retires_pending_journal_without_rolling_data_back() {
    let mut fixture = Fixture::new("commit-clear-crash");
    let data_file = fixture.source.join("book.txt");
    fs::write(&data_file, b"v1").unwrap();
    fixture.apply().unwrap();

    let backups = BackupStore::new(
        fixture.state_root.join("backups"),
        fixture.manifest.id.clone(),
    );
    let snapshot = backups
        .snapshot(Some(1), 2, std::slice::from_ref(&fixture.source))
        .unwrap();
    publish_pending(
        &fixture,
        &snapshot,
        Some(1),
        2,
        vec![fixture.source.clone()],
    );
    fs::write(&data_file, b"committed-v2").unwrap();
    SchemaStateStore::open(&fixture.state_root, &fixture.manifest.id)
        .unwrap()
        .publish(&AppliedSchema {
            format: STATE_FORMAT,
            app_id: fixture.manifest.id.clone(),
            version: 2,
            backup: snapshot.name().to_owned(),
        })
        .unwrap();

    fixture.set_version(2);
    fixture.apply().unwrap();
    assert_eq!(fs::read(data_file).unwrap(), b"committed-v2");
    assert!(!fixture.state_root.join("pending.json").exists());
    assert!(fixture.state_root.join("backups/from-1-to-2").exists());
}

#[test]
fn rolled_back_runner_keeps_new_schema_agent_blocked_after_committed_pending_recovery() {
    let fixture = Fixture::new("committed-pending-downgrade");
    let data_file = fixture.source.join("book.txt");
    fs::write(&data_file, b"v1").unwrap();
    fixture.apply().unwrap();
    assert_eq!(ready_fence(&fixture.state_root), "test-app:1\n");

    let backups = BackupStore::new(
        fixture.state_root.join("backups"),
        fixture.manifest.id.clone(),
    );
    let snapshot = backups
        .snapshot(Some(1), 2, std::slice::from_ref(&fixture.source))
        .unwrap();
    publish_pending(
        &fixture,
        &snapshot,
        Some(1),
        2,
        vec![fixture.source.clone()],
    );
    fs::write(&data_file, b"committed-v2").unwrap();
    SchemaStateStore::open(&fixture.state_root, &fixture.manifest.id)
        .unwrap()
        .publish(&AppliedSchema {
            format: STATE_FORMAT,
            app_id: fixture.manifest.id.clone(),
            version: 2,
            backup: snapshot.name().to_owned(),
        })
        .unwrap();

    // Simulate power loss after v2 state.json committed but before pending.json
    // and the stale v1 fence were reconciled, followed by a v1 binary rollback.
    let downgrade = fixture.apply();
    assert!(matches!(
        downgrade,
        Err(DataSchemaError::Downgrade {
            applied: 2,
            requested: 1
        })
    ));
    assert_eq!(state_version(&fixture.state_root), 2);
    assert_eq!(fs::read(data_file).unwrap(), b"committed-v2");
    assert!(!fixture.state_root.join("pending.json").exists());
    assert_eq!(ready_fence(&fixture.state_root), "test-app:2\n");
}

#[test]
fn timed_out_migration_is_killed_restored_and_not_committed() {
    let mut fixture = Fixture::new("timeout");
    let data_file = fixture.source.join("book.txt");
    fs::write(&data_file, b"v1").unwrap();
    fixture.apply().unwrap();
    fixture.set_version(2);
    fixture
        .manifest
        .data_schema
        .as_mut()
        .unwrap()
        .migration_timeout_ms = 100;
    fixture.manifest.environment =
        BTreeMap::from([("TEST_DATA_FILE".into(), data_file.display().to_string())]);
    let script = write_script(
        &fixture.root,
        "migrate-timeout",
        "printf 'partial' > \"$TEST_DATA_FILE\"\nsleep 5",
    );
    fixture.set_migrator(script);
    fixture.refresh_environment();
    let started = Instant::now();
    let timed_out = fixture.apply();
    assert!(
        matches!(
            timed_out,
            Err(DataSchemaError::MigratorTimedOut {
                timeout_ms: 100,
                ..
            })
        ),
        "unexpected timeout result: {timed_out:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(state_version(&fixture.state_root), 1);
    assert_eq!(fs::read(&data_file).unwrap(), b"v1");
}

#[test]
fn downgrade_is_rejected_without_touching_data_or_state() {
    let mut fixture = Fixture::new("downgrade");
    fs::write(fixture.source.join("book.txt"), b"current").unwrap();
    fixture.set_version(2);
    fixture.apply().unwrap();
    fixture.set_version(1);
    let downgrade = fixture.apply();
    assert!(
        matches!(
            downgrade,
            Err(DataSchemaError::Downgrade {
                applied: 2,
                requested: 1
            })
        ),
        "unexpected downgrade result: {downgrade:?}"
    );
    assert_eq!(state_version(&fixture.state_root), 2);
    assert_eq!(
        fs::read(fixture.source.join("book.txt")).unwrap(),
        b"current"
    );
}

#[test]
fn concurrent_runner_cannot_enter_the_same_schema_transaction() {
    let mut fixture = Fixture::new("concurrent");
    let entered = fixture.root.join("entered");
    fixture.manifest.environment =
        BTreeMap::from([("TEST_ENTERED".into(), entered.display().to_string())]);
    let script = write_script(
        &fixture.root,
        "migrate-slow",
        "printf 'yes' > \"$TEST_ENTERED\"\nsleep 1",
    );
    fixture.set_migrator(script);
    fixture
        .manifest
        .data_schema
        .as_mut()
        .unwrap()
        .migration_timeout_ms = 2_000;
    fixture.refresh_environment();
    let manifest = Arc::new(fixture.manifest.clone());
    let environment = Arc::new(fixture.environment.clone());
    let state_root = Arc::new(fixture.state_root.clone());
    let first_manifest = Arc::clone(&manifest);
    let first_environment = Arc::clone(&environment);
    let first_root = Arc::clone(&state_root);
    let first = std::thread::spawn(move || {
        apply_at(
            &first_manifest,
            first_manifest.data_schema.as_ref().unwrap(),
            &first_environment,
            &first_root,
        )
    });
    for _ in 0..100 {
        if entered.exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(entered.exists());
    assert!(matches!(
        apply_at(
            &manifest,
            manifest.data_schema.as_ref().unwrap(),
            &environment,
            &state_root,
        ),
        Err(DataSchemaError::ConcurrentTransaction)
    ));
    first.join().unwrap().unwrap();
    assert_eq!(state_version(&fixture.state_root), 1);
}

#[test]
fn schema_state_cannot_live_inside_a_declared_backup_tree() {
    let mut fixture = Fixture::new("recursive");
    fixture.manifest.data_schema.as_mut().unwrap().backup_paths = vec![fixture.root.join("home")];
    assert!(matches!(
        fixture.apply(),
        Err(DataSchemaError::RecursiveBackup { .. })
    ));
    assert!(!fixture.state_root.exists());
}

#[test]
fn declared_backup_cannot_live_inside_the_managed_schema_tree() {
    let mut fixture = Fixture::new("recursive-child");
    fixture.manifest.data_schema.as_mut().unwrap().backup_paths =
        vec![fixture.state_root.join("nested-source")];
    assert!(matches!(
        fixture.apply(),
        Err(DataSchemaError::RecursiveBackup { .. })
    ));
    assert!(!fixture.state_root.exists());
}

#[test]
fn corrupted_state_is_not_treated_as_an_uninstalled_schema() {
    let fixture = Fixture::new("corrupt-state");
    fs::create_dir_all(&fixture.state_root).unwrap();
    fs::set_permissions(&fixture.state_root, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(fixture.state_root.join("state.json"), b"not json").unwrap();
    fs::set_permissions(
        fixture.state_root.join("state.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    assert!(matches!(
        fixture.apply(),
        Err(DataSchemaError::InvalidState(_))
    ));
}

#[test]
fn manifest_identity_used_by_test_fixture_is_stable() {
    assert_eq!(AppId::new("test-app").unwrap().as_str(), "test-app");
}

#[test]
fn schema_phase_markers_are_atomic_and_bound_to_the_launch_generation() {
    let fixture = Fixture::new("phase-markers");
    let runtime = fixture.root.join("runtime");
    let plan = ExecutionPlan {
        generation: Some(41),
        variables: fixture.environment.variables.clone(),
        launch_environment: Some(fixture.environment.clone()),
        clear_inherited_environment: true,
    };
    apply(&fixture.manifest, &plan).unwrap();
    assert_eq!(
        fs::read(runtime.join(SCHEMA_PREPARED_FILE)).unwrap(),
        b"41\n"
    );
    assert_eq!(
        fs::read(runtime.join(SCHEMA_COMPLETE_FILE)).unwrap(),
        b"41\n"
    );
}
