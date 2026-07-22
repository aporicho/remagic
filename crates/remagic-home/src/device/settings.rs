use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const CONFIG_PATH: &str = "/home/root/.config/remagic/home.toml";
const WALLPAPER_DIR: &str = "/home/root/.local/share/remagic/wallpapers";
const SYSTEM_WALLPAPER: &str = "/usr/share/remarkable/suspended.png";
const DEFAULT_WALLPAPER_ID: &str = "default";
const SYSTEM_WALLPAPER_ID: &str = "system";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum WallpaperFit {
    #[default]
    Cover,
    Contain,
}

impl WallpaperFit {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Cover => "填充裁切",
            Self::Contain => "完整适应",
        }
    }

    pub(super) fn toggle(&mut self) {
        *self = match self {
            Self::Cover => Self::Contain,
            Self::Contain => Self::Cover,
        };
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(super) struct LockSettings {
    pub(super) wallpaper: String,
    pub(super) fit: WallpaperFit,
    pub(super) show_clock: bool,
    pub(super) show_hint: bool,
}

impl Default for LockSettings {
    fn default() -> Self {
        Self {
            wallpaper: DEFAULT_WALLPAPER_ID.into(),
            fit: WallpaperFit::Cover,
            show_clock: true,
            show_hint: true,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub(super) struct HomeSettings {
    pub(super) lock: LockSettings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WallpaperOption {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) path: Option<PathBuf>,
}

impl HomeSettings {
    pub(super) fn load() -> Self {
        match load_from(Path::new(CONFIG_PATH)) {
            Ok(settings) => settings,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                eprintln!("remagic-home: settings ignored: {error}");
                Self::default()
            }
        }
    }

    pub(super) fn save(&self) -> io::Result<()> {
        save_to(Path::new(CONFIG_PATH), self)
    }

    pub(super) fn cycle_wallpaper(&mut self, options: &[WallpaperOption]) {
        if options.is_empty() {
            self.lock.wallpaper = DEFAULT_WALLPAPER_ID.into();
            return;
        }
        let current = options
            .iter()
            .position(|option| option.id == self.lock.wallpaper)
            .unwrap_or(0);
        self.lock.wallpaper = options[(current + 1) % options.len()].id.clone();
    }

    pub(super) fn wallpaper<'a>(&self, options: &'a [WallpaperOption]) -> &'a WallpaperOption {
        options
            .iter()
            .find(|option| option.id == self.lock.wallpaper)
            .unwrap_or(&options[0])
    }
}

pub(super) fn ensure_wallpaper_dir() {
    if let Err(error) = fs::create_dir_all(WALLPAPER_DIR) {
        eprintln!("remagic-home: cannot create wallpaper directory: {error}");
        return;
    }
    let _ = fs::set_permissions(WALLPAPER_DIR, fs::Permissions::from_mode(0o755));
}

pub(super) fn wallpapers() -> Vec<WallpaperOption> {
    wallpaper_options(Path::new(WALLPAPER_DIR), Path::new(SYSTEM_WALLPAPER))
}

fn wallpaper_options(directory: &Path, system_path: &Path) -> Vec<WallpaperOption> {
    let mut options = vec![WallpaperOption {
        id: DEFAULT_WALLPAPER_ID.into(),
        label: "默认白纸".into(),
        path: None,
    }];
    if is_regular_file(system_path) {
        options.push(WallpaperOption {
            id: SYSTEM_WALLPAPER_ID.into(),
            label: "原生锁屏".into(),
            path: Some(system_path.to_owned()),
        });
    }
    let Ok(entries) = fs::read_dir(directory) else {
        return options;
    };
    let mut custom = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !is_regular_file(&path) || !has_png_extension(&path) {
                return None;
            }
            let file_name = path.file_name()?.to_str()?.to_owned();
            let label = path.file_stem()?.to_str()?.to_owned();
            Some(WallpaperOption {
                id: format!("custom:{file_name}"),
                label: shorten_label(&label, 18),
                path: Some(path),
            })
        })
        .collect::<Vec<_>>();
    custom.sort_by(|left, right| left.label.cmp(&right.label));
    options.extend(custom);
    options
}

fn has_png_extension(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("png"))
}

fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
}

fn shorten_label(label: &str, limit: usize) -> String {
    let mut chars = label.chars();
    let mut shortened = chars.by_ref().take(limit).collect::<String>();
    if chars.next().is_some() {
        shortened.push('…');
    }
    shortened
}

fn load_from(path: &Path) -> io::Result<HomeSettings> {
    let bytes = fs::read(path)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    toml::from_str(text).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn save_to(path: &Path, settings: &HomeSettings) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "settings path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let temporary = parent.join(format!(".home.toml.tmp-{}", std::process::id()));
    let encoded = toml::to_string_pretty(settings)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(encoded.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "remagic-home-settings-{name}-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ))
    }

    #[test]
    fn settings_round_trip_and_unknown_fields_use_defaults() {
        let root = temporary_directory("round-trip");
        let path = root.join("home.toml");
        let mut settings = HomeSettings::default();
        settings.lock.fit = WallpaperFit::Contain;
        settings.lock.show_clock = false;
        save_to(&path, &settings).unwrap();
        assert_eq!(load_from(&path).unwrap(), settings);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn wallpaper_catalog_rejects_symlinks_and_cycles_deterministically() {
        use std::os::unix::fs::symlink;

        let root = temporary_directory("wallpapers");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("乙.png"), b"not-decoded-by-catalog").unwrap();
        fs::write(root.join("甲.PNG"), b"not-decoded-by-catalog").unwrap();
        fs::write(root.join("ignored.jpg"), b"ignored").unwrap();
        symlink(root.join("乙.png"), root.join("linked.png")).unwrap();
        let options = wallpaper_options(&root, &root.join("missing-system.png"));
        assert_eq!(options.len(), 3);
        assert_eq!(options[0].id, DEFAULT_WALLPAPER_ID);
        assert_eq!(options[1].label, "乙");
        assert_eq!(options[2].label, "甲");

        let mut settings = HomeSettings::default();
        settings.cycle_wallpaper(&options);
        assert_eq!(settings.lock.wallpaper, options[1].id);
        settings.cycle_wallpaper(&options);
        assert_eq!(settings.lock.wallpaper, options[2].id);
        settings.cycle_wallpaper(&options);
        assert_eq!(settings.lock.wallpaper, DEFAULT_WALLPAPER_ID);
        fs::remove_dir_all(root).unwrap();
    }
}
