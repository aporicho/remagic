use remagic_core::BacklightSnapshot;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::warn;

const DEFAULT_CONFIG: &str = "/home/root/.config/remagic/backlight.toml";
const DEFAULT_SYSFS_ROOT: &str = "/sys/class/backlight";
const FRONTLIGHT_PROVIDER: &str = "rm_frontlight";
const BACKLIGHT_SETTINGS_SCHEMA: u32 = 1;
const BACKLIGHT_OFF_POWER: u32 = 4;
const BACKLIGHT_ON_POWER: u32 = 0;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct BacklightSettings {
    schema: u32,
    desired_percent: u8,
}

impl Default for BacklightSettings {
    fn default() -> Self {
        Self {
            schema: BACKLIGHT_SETTINGS_SCHEMA,
            desired_percent: 0,
        }
    }
}

impl BacklightSettings {
    fn validate(&self) -> Result<(), String> {
        if self.schema != BACKLIGHT_SETTINGS_SCHEMA {
            return Err(format!(
                "unsupported backlight settings schema {}",
                self.schema
            ));
        }
        if self.desired_percent > 100 {
            return Err("backlight percent must be between 0 and 100".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct BacklightProvider {
    name: String,
    path: PathBuf,
}

impl BacklightProvider {
    fn detect(root: &Path) -> Option<Self> {
        let path = root.join(FRONTLIGHT_PROVIDER);
        let has_required_files = path.join("brightness").is_file()
            && path.join("max_brightness").is_file()
            && path.join("bl_power").is_file();
        has_required_files.then(|| Self {
            name: FRONTLIGHT_PROVIDER.into(),
            path,
        })
    }

    fn read_snapshot(&self, desired_percent: u8, forced_off: bool) -> BacklightSnapshot {
        let max_brightness = read_u32(&self.path.join("max_brightness"));
        let brightness = read_u32(
            &self
                .path
                .join("actual_brightness")
                .is_file()
                .then(|| self.path.join("actual_brightness"))
                .unwrap_or_else(|| self.path.join("brightness")),
        );
        let bl_power = read_u32(&self.path.join("bl_power"));
        let percent = max_brightness
            .as_ref()
            .ok()
            .filter(|max| **max != 0)
            .map(|_| desired_percent);
        let mut error = None;
        for result in [&max_brightness, &brightness, &bl_power] {
            if let Err(read_error) = result {
                error = Some(read_error.clone());
                break;
            }
        }
        BacklightSnapshot {
            supported: error.is_none(),
            percent,
            forced_off,
            provider: Some(self.name.clone()),
            brightness: brightness.as_ref().ok().copied(),
            max_brightness: max_brightness.as_ref().ok().copied(),
            bl_power: bl_power.as_ref().ok().copied(),
            linear_mapping: read_optional_string(&self.path.join("linear_mapping")),
            error,
        }
    }

    fn current_percent(&self) -> Result<u8, String> {
        let max = read_u32(&self.path.join("max_brightness"))?;
        if max == 0 {
            return Err("frontlight max_brightness is zero".into());
        }
        let brightness = read_u32(
            &self
                .path
                .join("actual_brightness")
                .is_file()
                .then(|| self.path.join("actual_brightness"))
                .unwrap_or_else(|| self.path.join("brightness")),
        )?;
        Ok(percent_from_native(brightness, max))
    }

    fn apply_percent(&self, percent: u8) -> Result<(), String> {
        let max = read_u32(&self.path.join("max_brightness"))?;
        if max == 0 {
            return Err("frontlight max_brightness is zero".into());
        }
        if percent == 0 {
            write_u32(&self.path.join("brightness"), 0)?;
            write_u32(&self.path.join("bl_power"), BACKLIGHT_OFF_POWER)?;
        } else {
            write_u32(&self.path.join("bl_power"), BACKLIGHT_ON_POWER)?;
            write_u32(
                &self.path.join("brightness"),
                native_from_percent(percent, max),
            )?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct BacklightState {
    provider: Option<BacklightProvider>,
    desired_percent: u8,
    forced_off: bool,
    last_error: Option<String>,
}

pub struct BacklightManager {
    config_path: PathBuf,
    sysfs_root: PathBuf,
    state: Mutex<BacklightState>,
}

impl BacklightManager {
    pub fn load() -> Self {
        let config_path = std::env::var_os("REMAGIC_BACKLIGHT_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
        let sysfs_root = std::env::var_os("REMAGIC_BACKLIGHT_SYSFS")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SYSFS_ROOT));
        Self::load_at(config_path, sysfs_root)
    }

    fn load_at(config_path: PathBuf, sysfs_root: PathBuf) -> Self {
        let provider = BacklightProvider::detect(&sysfs_root);
        let desired_percent = match load_settings(&config_path) {
            Ok(Some(settings)) => settings.desired_percent,
            Ok(None) => provider
                .as_ref()
                .and_then(|provider| provider.current_percent().ok())
                .unwrap_or(0),
            Err(error) => {
                warn!(%error, path = %config_path.display(), "backlight settings ignored");
                provider
                    .as_ref()
                    .and_then(|provider| provider.current_percent().ok())
                    .unwrap_or(0)
            }
        };
        Self {
            config_path,
            sysfs_root,
            state: Mutex::new(BacklightState {
                provider,
                desired_percent,
                forced_off: false,
                last_error: None,
            }),
        }
    }

    pub fn snapshot(&self) -> BacklightSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        self.snapshot_locked(&state)
    }

    pub fn set_percent(&self, percent: u8) -> Result<BacklightSnapshot, String> {
        if percent > 100 {
            return Err("backlight percent must be between 0 and 100".into());
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(provider) = state.provider.clone() else {
            let message = format!(
                "{} was not found under {}",
                FRONTLIGHT_PROVIDER,
                self.sysfs_root.display()
            );
            state.last_error = Some(message.clone());
            return Err(message);
        };
        if let Err(error) = provider.apply_percent(percent) {
            state.last_error = Some(error.clone());
            return Err(error);
        }
        let settings = BacklightSettings {
            schema: BACKLIGHT_SETTINGS_SCHEMA,
            desired_percent: percent,
        };
        if let Err(error) = save_settings(&self.config_path, &settings) {
            state.last_error = Some(error.clone());
            return Err(error);
        }
        state.desired_percent = percent;
        state.forced_off = false;
        state.last_error = None;
        Ok(self.snapshot_locked(&state))
    }

    pub fn force_off(&self, reason: &str) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(provider) = state.provider.clone() else {
            state.forced_off = false;
            state.last_error = Some(format!(
                "cannot force frontlight off for {reason}: {FRONTLIGHT_PROVIDER} is unavailable"
            ));
            return;
        };
        match provider.apply_percent(0) {
            Ok(()) => {
                state.forced_off = true;
                state.last_error = None;
            }
            Err(error) => {
                state.last_error =
                    Some(format!("cannot force frontlight off for {reason}: {error}"));
            }
        }
    }

    pub fn restore_desired(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(provider) = state.provider.clone() else {
            state.forced_off = false;
            state.last_error = Some(format!("{FRONTLIGHT_PROVIDER} is unavailable"));
            return;
        };
        match provider.apply_percent(state.desired_percent) {
            Ok(()) => {
                state.forced_off = false;
                state.last_error = None;
            }
            Err(error) => {
                state.last_error = Some(format!("cannot restore frontlight: {error}"));
            }
        }
    }

    fn snapshot_locked(&self, state: &BacklightState) -> BacklightSnapshot {
        let Some(provider) = &state.provider else {
            return BacklightSnapshot::unsupported(format!(
                "{} was not found under {}",
                FRONTLIGHT_PROVIDER,
                self.sysfs_root.display()
            ));
        };
        let mut snapshot = provider.read_snapshot(state.desired_percent, state.forced_off);
        if snapshot.error.is_none() {
            snapshot.error = state.last_error.clone();
        }
        snapshot
    }
}

fn native_from_percent(percent: u8, max: u32) -> u32 {
    if percent == 0 || max == 0 {
        return 0;
    }
    (((percent as u64 * max as u64) + 50) / 100)
        .clamp(1, max as u64)
        .try_into()
        .unwrap_or(max)
}

fn percent_from_native(brightness: u32, max: u32) -> u8 {
    if max == 0 {
        return 0;
    }
    (((brightness.min(max) as u64 * 100) + max as u64 / 2) / max as u64)
        .min(100)
        .try_into()
        .unwrap_or(100)
}

fn read_u32(path: &Path) -> Result<u32, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    text.trim()
        .parse()
        .map_err(|_| format!("{} does not contain an unsigned integer", path.display()))
}

fn write_u32(path: &Path, value: u32) -> Result<(), String> {
    fs::write(path, format!("{value}\n"))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

fn read_optional_string(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
}

fn load_settings(path: &Path) -> Result<Option<BacklightSettings>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let settings: BacklightSettings = toml::from_str(&text)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
    settings.validate()?;
    Ok(Some(settings))
}

fn save_settings(path: &Path, settings: &BacklightSettings) -> Result<(), String> {
    settings.validate()?;
    let parent = path
        .parent()
        .ok_or_else(|| format!("backlight settings path {} has no parent", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot protect {}: {error}", parent.display()))?;
    let temporary = parent.join(format!(".backlight.toml.tmp-{}", std::process::id()));
    let encoded = toml::to_string_pretty(settings)
        .map_err(|error| format!("cannot encode backlight settings: {error}"))?;
    let result = (|| -> Result<(), String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
        file.write_all(encoded.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|error| format!("cannot commit {}: {error}", temporary.display()))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("cannot replace {}: {error}", path.display()))?;
        fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("cannot sync {}: {error}", parent.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests;
