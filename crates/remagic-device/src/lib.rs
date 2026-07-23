//! Strict, shared device detection for the Paper Pro family.
//!
//! ReMagic deliberately supports an allow-list rather than guessing from
//! framebuffer dimensions. The same profile is used by the installer, display
//! host, manager and application runner so an application never selects a
//! device-specific QTFB format itself.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const DEVICE_PROFILE_SCHEMA_V1: u32 = 1;
pub const DEVICE_PROFILE_ENV: &str = "REMAGIC_DEVICE_PROFILE";
/// The reMarkable software release family covered by this ReMagic build.
///
/// `/etc/os-release` exposes the user-facing software build as `IMG_VERSION`;
/// `VERSION_ID` is the underlying Codex Linux image and must not be used for
/// application compatibility decisions.
pub const SUPPORTED_OS_SERIES: &str = "3.27";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceProduct {
    PaperPro,
    PaperProMove,
}

impl DeviceProduct {
    pub const fn codename(self) -> &'static str {
        match self {
            Self::PaperPro => "ferrari",
            Self::PaperProMove => "chiappa",
        }
    }

    pub const fn machine(self) -> &'static str {
        match self {
            Self::PaperPro => "reMarkable Ferrari",
            Self::PaperProMove => "reMarkable Chiappa",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfacePixelFormat {
    Rgb565,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceDisplayProfile {
    pub logical_width: i32,
    pub logical_height: i32,
    pub qtfb_format: u8,
    pub pixel_format: SurfacePixelFormat,
    pub stride: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceProfile {
    pub schema_version: u32,
    pub product: DeviceProduct,
    pub codename: String,
    pub os_version: String,
    pub display: DeviceDisplayProfile,
    pub capabilities: Vec<String>,
}

impl DeviceProfile {
    pub fn for_product(product: DeviceProduct, os_version: impl Into<String>) -> Self {
        let (logical_width, logical_height, qtfb_format) = match product {
            DeviceProduct::PaperPro => (1620, 2160, 3),
            DeviceProduct::PaperProMove => (954, 1696, 6),
        };
        Self {
            schema_version: DEVICE_PROFILE_SCHEMA_V1,
            product,
            codename: product.codename().into(),
            os_version: os_version.into(),
            display: DeviceDisplayProfile {
                logical_width,
                logical_height,
                qtfb_format,
                pixel_format: SurfacePixelFormat::Rgb565,
                stride: logical_width as usize * 2,
            },
            capabilities: [
                "color",
                "display:qtfb-v1",
                "display:surface-v2",
                "ink:direct-v1",
                "input:mode-v2",
                "input:pen-v1",
                "input:touch-v1",
                "lifecycle:v2",
                "network:outbound-v1",
                "network:listen-v1",
                "storage:books-write-v1",
                "storage:wallpapers-write-v1",
                "agent:pi-v1",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        }
    }

    pub fn detect() -> Result<Self, DeviceProfileError> {
        Self::detect_at(Path::new("/"))
    }

    pub fn detect_at(root: &Path) -> Result<Self, DeviceProfileError> {
        let machine_path = rooted(root, "sys/devices/soc0/machine");
        let model_path = rooted(root, "proc/device-tree/model");
        let machine = read_identity(&machine_path)?;
        let model = read_identity(&model_path)?;
        if machine != model {
            return Err(DeviceProfileError::IdentityMismatch { machine, model });
        }
        let product = match machine.as_str() {
            "reMarkable Ferrari" => DeviceProduct::PaperPro,
            "reMarkable Chiappa" => DeviceProduct::PaperProMove,
            _ => return Err(DeviceProfileError::UnsupportedMachine(machine)),
        };
        let os_release_path = rooted(root, "etc/os-release");
        let os_release =
            fs::read_to_string(&os_release_path).map_err(|source| DeviceProfileError::Read {
                path: os_release_path,
                source,
            })?;
        let os_version =
            parse_os_version(&os_release).ok_or(DeviceProfileError::MissingOsVersion)?;
        if !is_supported_os_version(&os_version) {
            return Err(DeviceProfileError::UnsupportedOsVersion {
                detected: os_version,
                supported: SUPPORTED_OS_SERIES,
            });
        }
        let profile = Self::for_product(product, os_version);
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), DeviceProfileError> {
        if self.schema_version != DEVICE_PROFILE_SCHEMA_V1 {
            return Err(DeviceProfileError::UnsupportedSchema(self.schema_version));
        }
        if self.codename != self.product.codename() || self.os_version.trim().is_empty() {
            return Err(DeviceProfileError::InvalidProfile);
        }
        let expected = Self::for_product(self.product, self.os_version.clone());
        if self.display != expected.display || self.capabilities != expected.capabilities {
            return Err(DeviceProfileError::InvalidProfile);
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, DeviceProfileError> {
        self.validate()?;
        serde_json::to_string(self).map_err(DeviceProfileError::Serialize)
    }
}

fn rooted(root: &Path, relative: &str) -> PathBuf {
    if root == Path::new("/") {
        Path::new("/").join(relative)
    } else {
        root.join(relative)
    }
}

fn read_identity(path: &Path) -> Result<String, DeviceProfileError> {
    let bytes = fs::read(path).map_err(|source| DeviceProfileError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let value = String::from_utf8(bytes)
        .map_err(|_| DeviceProfileError::InvalidIdentity(path.to_path_buf()))?
        .trim_matches(|character: char| character == '\0' || character.is_whitespace())
        .to_owned();
    if value.is_empty() {
        Err(DeviceProfileError::InvalidIdentity(path.to_path_buf()))
    } else {
        Ok(value)
    }
}

pub fn is_supported_os_version(version: &str) -> bool {
    version == SUPPORTED_OS_SERIES
        || version
            .strip_prefix(SUPPORTED_OS_SERIES)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

fn parse_os_version(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        (candidate == "IMG_VERSION")
            .then(|| value.trim().trim_matches('"').to_owned())
            .filter(|value| !value.is_empty())
    })
}

#[derive(Debug, Error)]
pub enum DeviceProfileError {
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("device identity is not valid UTF-8 or is empty: {0}")]
    InvalidIdentity(PathBuf),
    #[error("device identity mismatch: machine={machine:?}, model={model:?}")]
    IdentityMismatch { machine: String, model: String },
    #[error("unsupported reMarkable machine: {0}")]
    UnsupportedMachine(String),
    #[error("/etc/os-release has no IMG_VERSION")]
    MissingOsVersion,
    #[error("unsupported reMarkable software {detected}; this build supports {supported}.x")]
    UnsupportedOsVersion {
        detected: String,
        supported: &'static str,
    },
    #[error("unsupported device profile schema {0}")]
    UnsupportedSchema(u32),
    #[error("device profile fields are internally inconsistent")]
    InvalidProfile,
    #[error("could not serialize device profile: {0}")]
    Serialize(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    fn fixture(machine: &str) -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "remagic-device-{}-{id}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("sys/devices/soc0")).unwrap();
        fs::create_dir_all(root.join("proc/device-tree")).unwrap();
        fs::create_dir_all(root.join("etc")).unwrap();
        fs::write(root.join("sys/devices/soc0/machine"), machine).unwrap();
        fs::write(root.join("proc/device-tree/model"), format!("{machine}\0")).unwrap();
        fs::write(
            root.join("etc/os-release"),
            "ID=codex\nVERSION_ID=5.7.126\nIMG_VERSION=3.27.3.0\n",
        )
        .unwrap();
        root
    }

    #[test]
    fn both_supported_products_have_exact_surface_contracts() {
        let ferrari = DeviceProfile::for_product(DeviceProduct::PaperPro, "5.7.126");
        assert_eq!(
            (
                ferrari.display.logical_width,
                ferrari.display.logical_height
            ),
            (1620, 2160)
        );
        assert_eq!(ferrari.display.qtfb_format, 3);
        let chiappa = DeviceProfile::for_product(DeviceProduct::PaperProMove, "5.7.126");
        assert_eq!(
            (
                chiappa.display.logical_width,
                chiappa.display.logical_height
            ),
            (954, 1696)
        );
        assert_eq!(chiappa.display.qtfb_format, 6);
    }

    #[test]
    fn detection_cross_checks_machine_model_and_os() {
        for (machine, product) in [
            ("reMarkable Ferrari", DeviceProduct::PaperPro),
            ("reMarkable Chiappa", DeviceProduct::PaperProMove),
        ] {
            let root = fixture(machine);
            let profile = DeviceProfile::detect_at(&root).unwrap();
            assert_eq!(profile.product, product);
            assert_eq!(profile.os_version, "3.27.3.0");
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn conflicting_identity_fails_closed() {
        let root = fixture("reMarkable Ferrari");
        fs::write(root.join("proc/device-tree/model"), "reMarkable Chiappa\0").unwrap();
        assert!(matches!(
            DeviceProfile::detect_at(&root),
            Err(DeviceProfileError::IdentityMismatch { .. })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn kernel_image_version_is_never_mistaken_for_software_version() {
        let root = fixture("reMarkable Ferrari");
        fs::write(
            root.join("etc/os-release"),
            "VERSION_ID=5.7.126\nIMG_VERSION=3.26.2.1\n",
        )
        .unwrap();
        assert!(matches!(
            DeviceProfile::detect_at(&root),
            Err(DeviceProfileError::UnsupportedOsVersion { detected, .. })
                if detected == "3.26.2.1"
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_the_current_stable_release_series_is_supported() {
        assert!(is_supported_os_version("3.27"));
        assert!(is_supported_os_version("3.27.3.0"));
        assert!(!is_supported_os_version("3.270.0.0"));
        assert!(!is_supported_os_version("3.28.0.0"));
    }
}
