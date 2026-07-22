use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn env_path(key: &str, fallback: &str) -> PathBuf {
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback.into())
}

pub(super) fn runtime_generation_matches(
    generations: &BTreeMap<AppId, u64>,
    app: &AppId,
    generation: u64,
) -> bool {
    generation != 0 && generations.get(app).copied() == Some(generation)
}

pub(super) fn app_unit(app: &AppId) -> String {
    format!("remagic-app@{}.service", app.as_str())
}

pub(super) fn runtime_exit_is_crash(exit_code: i32, reported_crash: bool) -> bool {
    reported_crash || exit_code != 0
}

pub(super) async fn wait_readiness_file(
    path: &Path,
    timeout: std::time::Duration,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "readiness file {} was not published within {} ms",
                path.display(),
                timeout.as_millis()
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

pub(super) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0)
}

pub(super) fn atomic_write_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let temp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    fs::write(&temp, bytes).map_err(|e| e.to_string())?;
    fs::rename(&temp, path).map_err(|e| e.to_string())
}

pub(super) fn set_foreground_marker(app: Option<&AppId>) -> Result<(), String> {
    match app {
        Some(app) => fs::write(FOREGROUND_MARKER, format!("{}\n", app.as_str()))
            .map_err(|error| error.to_string()),
        None => match fs::remove_file(FOREGROUND_MARKER) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_magicpaper_manifest_satisfies_v2_contract() {
        let manifest: remagic_core::AppManifest =
            toml::from_str(include_str!("../../../../manifests/magicpaper.toml")).unwrap();
        manifest.validate().unwrap();
        assert_eq!(manifest.schema, remagic_core::MANIFEST_SCHEMA_V2);
        assert!(manifest.is_resident());
        assert_eq!(
            manifest.runtime_profile(),
            remagic_core::RuntimeProfile::QtfbCompat
        );
    }

    #[test]
    fn bundled_store_manifest_is_a_non_removable_system_application() {
        let manifest: remagic_core::AppManifest =
            toml::from_str(include_str!("../../../../manifests/remagic-store.toml")).unwrap();
        manifest.validate().unwrap();
        assert_eq!(manifest.name, "应用商店");
        assert_eq!(manifest.kind, remagic_core::AppKind::System);
        assert_eq!(manifest.id.as_str(), "remagic-store");
    }

    #[test]
    fn runtime_generation_fences_replacements() {
        let app = AppId::new("koreader").unwrap();
        let generations = BTreeMap::from([(app.clone(), 8)]);
        assert!(!runtime_generation_matches(&generations, &app, 7));
        assert!(runtime_generation_matches(&generations, &app, 8));
        assert!(!runtime_generation_matches(&generations, &app, 0));
        assert!(!runtime_generation_matches(&BTreeMap::new(), &app, 1));
    }

    #[test]
    fn only_zero_normal_exit_is_clean() {
        assert!(!runtime_exit_is_crash(0, false));
        assert!(runtime_exit_is_crash(1, false));
        assert!(runtime_exit_is_crash(0, true));
    }
}
