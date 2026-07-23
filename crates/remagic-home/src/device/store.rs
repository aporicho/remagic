use remagic_core::{DeviceProduct, DeviceProfile};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

const STORE_BINARY: &str = "/home/root/apps/remagic-store/current/bin/remagic-store";

#[derive(Clone, Debug, Default)]
pub(super) struct CatalogApp {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) summary: String,
    pub(super) version: String,
}

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
            })
        })
        .collect())
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

    #[tokio::test]
    async fn unknown_catalog_identity_fails_before_process_launch() {
        assert!(install("koreader-for-remagic").await.is_err());
    }
}
