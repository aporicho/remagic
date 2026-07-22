use super::LaunchContext;
use remagic_core::{SCHEMA_COMMIT_GRACE_MS, SCHEMA_COMPLETE_FILE, SCHEMA_PREPARED_FILE};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

pub(super) fn clear_phase_markers(context: &LaunchContext) -> Result<(), String> {
    if context.manifest.data_schema.is_none() {
        return Ok(());
    }
    for name in [SCHEMA_PREPARED_FILE, SCHEMA_COMPLETE_FILE] {
        let path = context.runtime_dir.join(name);
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not clear stale schema phase {}: {error}",
                    path.display()
                ));
            }
        }
    }
    Ok(())
}

pub(super) async fn wait_phase(
    context: &LaunchContext,
    name: &str,
    timeout: Duration,
) -> Result<(), String> {
    let path = context.runtime_dir.join(name);
    let expected = format!("{}\n", context.generation);
    let started = Instant::now();
    loop {
        if phase_matches(&path, &expected)? {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "schema phase {name} exceeded its {} ms deadline",
                timeout.as_millis()
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(super) fn background_restore_is_safe(context: &LaunchContext) -> bool {
    !context.background_quiesced
        || phase_matches(
            &context.runtime_dir.join(SCHEMA_COMPLETE_FILE),
            &format!("{}\n", context.generation),
        )
        .unwrap_or(false)
}

pub(super) fn startup_budgets(
    context: &LaunchContext,
) -> (Option<Duration>, Option<Duration>, Duration, Duration) {
    let readiness = Duration::from_millis(context.manifest.readiness.timeout_ms.max(1_000));
    let schema = (!context.active)
        .then_some(context.manifest.data_schema.as_ref())
        .flatten();
    let backup = schema.map(|value| Duration::from_millis(value.backup_timeout_ms));
    let migration = schema.map(|value| {
        Duration::from_millis(
            value
                .migration_timeout_ms
                .saturating_add(SCHEMA_COMMIT_GRACE_MS),
        )
    });
    (backup, migration, readiness, readiness)
}

fn phase_matches(path: &Path, expected: &str) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && metadata.is_file() => {
            let value = fs::read_to_string(path).map_err(|error| {
                format!("could not read schema phase {}: {error}", path.display())
            })?;
            if value == expected {
                Ok(true)
            } else {
                Err(format!(
                    "schema phase {} has a stale generation",
                    path.display()
                ))
            }
        }
        Ok(_) => Err(format!(
            "schema phase is not a regular file: {}",
            path.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!(
            "could not inspect schema phase {}: {error}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn context(active: bool) -> LaunchContext {
        let mut manifest: remagic_core::AppManifest =
            toml::from_str(include_str!("../../../../../manifests/magicpaper.toml")).unwrap();
        manifest.readiness.timeout_ms = 20_000;
        let background_execution = manifest.runtime.background_execution;
        LaunchContext {
            id: manifest.id.clone(),
            manifest,
            open_path: None,
            resume_payload: None,
            runtime_dir: "/run/remagic/apps/magicpaper".into(),
            unit: "remagic-app@magicpaper.service".into(),
            active,
            generation: 1,
            background_execution,
            foreground_epoch: 1,
            lease_id: 1,
            surface_key: 1,
            launch_path: "/run/remagic/launch/magicpaper.json".into(),
            background_quiesced: !active,
        }
    }

    #[test]
    fn cold_launch_keeps_each_schema_and_surface_budget_independent() {
        assert_eq!(
            startup_budgets(&context(false)),
            (
                Some(Duration::from_secs(120)),
                Some(Duration::from_secs(130)),
                Duration::from_secs(20),
                Duration::from_secs(20),
            )
        );
        assert_eq!(
            startup_budgets(&context(true)),
            (None, None, Duration::from_secs(20), Duration::from_secs(20),)
        );
    }

    #[test]
    fn phase_marker_requires_a_regular_file_with_the_exact_generation() {
        let root = std::env::temp_dir().join(format!(
            "remagic-schema-phase-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let marker = root.join("marker");
        assert!(!phase_matches(&marker, "7\n").unwrap());
        fs::write(&marker, b"7\n").unwrap();
        assert!(phase_matches(&marker, "7\n").unwrap());
        assert!(phase_matches(&marker, "8\n").is_err());
        fs::remove_file(&marker).unwrap();
        fs::write(root.join("target"), b"7\n").unwrap();
        symlink("target", &marker).unwrap();
        assert!(phase_matches(&marker, "7\n").is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn quiesced_background_writer_requires_its_exact_schema_complete_marker() {
        let root = std::env::temp_dir().join(format!(
            "remagic-schema-background-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut cold = context(false);
        cold.runtime_dir = root.clone();
        assert!(!background_restore_is_safe(&cold));
        fs::write(root.join(SCHEMA_COMPLETE_FILE), b"2\n").unwrap();
        assert!(!background_restore_is_safe(&cold));
        fs::write(root.join(SCHEMA_COMPLETE_FILE), b"1\n").unwrap();
        assert!(background_restore_is_safe(&cold));
        assert!(background_restore_is_safe(&context(true)));
        fs::remove_dir_all(root).unwrap();
    }
}
