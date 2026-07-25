use remagic_core::{DeviceProduct, DeviceProfile};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

const STORE_BINARY: &str = "/home/root/apps/remagic-store/current/payload/bin/remagic-store";

#[derive(Clone, Debug, Default)]
pub(super) struct CatalogApp {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) summary: String,
    pub(super) version: String,
    pub(super) status: CatalogStatus,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum CatalogStatus {
    #[default]
    NotInstalled,
    Installed,
    UpdateAvailable,
    NeedsConfiguration,
    Incompatible,
}

#[derive(Clone, Debug, Default)]
pub(super) struct SystemUpdateInfo {
    pub(super) current_version: String,
    pub(super) available_version: String,
    pub(super) update_available: bool,
}

#[derive(Clone, Debug)]
pub(super) struct OperationProgress {
    pub(super) target_id: String,
    pub(super) label: String,
    pub(super) fraction: Option<f32>,
}

#[derive(Debug)]
pub(super) enum TaskResult {
    Store {
        app_id: String,
        result: Result<(), String>,
    },
    SystemInstall {
        result: Result<(), String>,
    },
}

impl OperationProgress {
    pub(super) fn indeterminate(target_id: &str, label: &str) -> Self {
        Self {
            target_id: target_id.to_owned(),
            label: label.to_owned(),
            fraction: None,
        }
    }

    pub(super) fn complete(target_id: &str, label: &str) -> Self {
        Self {
            target_id: target_id.to_owned(),
            label: label.to_owned(),
            fraction: Some(1.0),
        }
    }
}

const SYSTEM_RELEASE_URL: &str =
    "https://github.com/aporicho/remagic/releases/latest/download/remagic-release-v1.json";
const SYSTEM_SIGNATURE_URL: &str =
    "https://github.com/aporicho/remagic/releases/latest/download/remagic-release-v1.sig.json";

pub(super) async fn catalog() -> Result<Vec<CatalogApp>, Box<dyn std::error::Error>> {
    let device = DeviceProfile::detect()?;
    let product = match device.product {
        DeviceProduct::PaperPro => "paper_pro",
        DeviceProduct::PaperProMove => "paper_pro_move",
    };
    let api = remagic_core::REMAGIC_APP_API_VERSION.to_string();
    let output = Command::new(STORE_BINARY)
        .args(["catalog", product, device.os_version.as_str(), &api])
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/home/root")
        .env("REMAGIC_API_VERSION", &api)
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_owned()
            .into());
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let apps = value
        .get("apps")
        .and_then(serde_json::Value::as_array)
        .ok_or("应用商店返回了无效目录")?;
    Ok(apps
        .iter()
        .filter_map(|app| {
            Some(CatalogApp {
                id: app.get("id")?.as_str()?.to_owned(),
                name: app.get("name")?.as_str()?.to_owned(),
                summary: app.get("summary")?.as_str()?.to_owned(),
                version: app.get("available_version")?.as_str()?.to_owned(),
                status: match app
                    .get("status")
                    .and_then(|status| status.get("status"))
                    .and_then(serde_json::Value::as_str)
                {
                    Some("installed") => CatalogStatus::Installed,
                    Some("update_available") => CatalogStatus::UpdateAvailable,
                    Some("needs_configuration") => CatalogStatus::NeedsConfiguration,
                    Some("incompatible") => CatalogStatus::Incompatible,
                    _ => CatalogStatus::NotInstalled,
                },
            })
        })
        .collect())
}

pub(super) async fn system_update_info() -> Result<SystemUpdateInfo, Box<dyn std::error::Error>> {
    let device = DeviceProfile::detect()?;
    let product = product_name(device.product);
    let minimum = installed_sequence();
    let output = Command::new("/home/root/apps/remagic/bin/remagic-update")
        .args([
            "check",
            SYSTEM_RELEASE_URL,
            SYSTEM_SIGNATURE_URL,
            product,
            device.os_version.as_str(),
            &minimum.to_string(),
        ])
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_owned()
            .into());
    }
    let release: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let available_version = release
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("未知")
        .to_owned();
    let sequence = release
        .get("sequence")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(minimum);
    Ok(SystemUpdateInfo {
        current_version: installed_version(),
        available_version,
        update_available: sequence > minimum,
    })
}

pub(super) async fn install_system_update() -> Result<(), Box<dyn std::error::Error>> {
    let device = DeviceProfile::detect()?;
    let product = product_name(device.product);
    let minimum = installed_sequence();
    let unit = format!("remagic-system-update-{}", std::process::id());
    let output = Command::new("systemd-run")
        .args([
            "--collect",
            "--unit",
            &unit,
            "/home/root/apps/remagic/bin/remagic-update",
            "install",
            SYSTEM_RELEASE_URL,
            SYSTEM_SIGNATURE_URL,
            product,
            device.os_version.as_str(),
            &minimum.to_string(),
        ])
        .output()
        .await?;
    output.status.success().then_some(()).ok_or_else(|| {
        String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_owned()
            .into()
    })
}

pub(super) async fn uninstall(app_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !matches!(app_id, "magicpaper" | "koreader" | "upload") {
        return Err(format!("应用商店没有这个应用：{app_id}").into());
    }
    let child = Command::new(STORE_BINARY)
        .args(["uninstall", app_id])
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/home/root")
        .env("REMAGIC_CONTROL_SOCKET", remagic_protocol::DEFAULT_SOCKET)
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(15 * 60), child)
        .await
        .map_err(|_| "应用卸载超时")??;
    output.status.success().then_some(()).ok_or_else(|| {
        String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_owned()
            .into()
    })
}

fn product_name(product: DeviceProduct) -> &'static str {
    match product {
        DeviceProduct::PaperPro => "paper_pro",
        DeviceProduct::PaperProMove => "paper_pro_move",
    }
}

fn installed_sequence() -> u64 {
    std::fs::read_to_string("/home/root/apps/remagic/share/release.env")
        .ok()
        .and_then(|text| manifest_value(&text, "REMAGIC_RELEASE_SEQUENCE"))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

fn installed_version() -> String {
    std::fs::read_to_string("/home/root/apps/remagic/share/release.env")
        .ok()
        .and_then(|text| manifest_value(&text, "REMAGIC_VERSION"))
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
}

fn manifest_value(text: &str, key: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")).map(str::to_owned))
}

pub(super) async fn install(app_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !matches!(app_id, "magicpaper" | "koreader" | "upload") {
        return Err(format!("应用商店没有这个应用：{app_id}").into());
    }
    if !Path::new(STORE_BINARY).is_file() {
        return Err("应用商店组件未安装，请先更新 ReMagic".into());
    }
    let device = DeviceProfile::detect()?;
    let product = match device.product {
        DeviceProduct::PaperPro => "paper_pro",
        DeviceProduct::PaperProMove => "paper_pro_move",
    };
    run_package_command("install", app_id, product, &device.os_version).await
}

pub(super) async fn upgrade(app_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !matches!(app_id, "magicpaper" | "koreader" | "upload") {
        return Err(format!("应用商店没有这个应用：{app_id}").into());
    }
    if !Path::new(STORE_BINARY).is_file() {
        return Err("应用商店组件未安装，请先更新 ReMagic".into());
    }
    let device = DeviceProfile::detect()?;
    let product = match device.product {
        DeviceProduct::PaperPro => "paper_pro",
        DeviceProduct::PaperProMove => "paper_pro_move",
    };
    run_package_command("upgrade", app_id, product, &device.os_version).await
}

async fn run_package_command(
    operation: &str,
    app_id: &str,
    product: &str,
    os_version: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let api = remagic_core::REMAGIC_APP_API_VERSION.to_string();
    let child = Command::new(STORE_BINARY)
        .args([operation, app_id, product, os_version, &api])
        .env_clear()
        .env(
            "PATH",
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
        )
        .env("HOME", "/home/root")
        .env("REMAGIC_API_VERSION", api)
        .env("REMAGIC_CONTROL_SOCKET", remagic_protocol::DEFAULT_SOCKET)
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(15 * 60), child)
        .await
        .map_err(|_| "应用安装超时")??;
    if output.status.success() {
        Ok(())
    } else {
        let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if message.is_empty() {
            format!("应用安装失败：{}", output.status).into()
        } else {
            message.into()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_binary_follows_the_transactional_package_layout() {
        assert_eq!(
            STORE_BINARY,
            "/home/root/apps/remagic-store/current/payload/bin/remagic-store"
        );
    }

    #[tokio::test]
    async fn unknown_catalog_identity_fails_before_process_launch() {
        assert!(install("koreader-for-remagic").await.is_err());
    }
}
