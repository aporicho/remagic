use remagic_core::{DeviceProduct, DeviceProfile};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

const STORE_BINARY: &str = "/home/root/apps/remagic-store/current/bin/remagic-store";

pub(super) async fn install(app_id: &str) -> Result<(), Box<dyn std::error::Error>> {
    if !matches!(app_id, "magicpaper" | "koreader") {
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
    let child = Command::new(STORE_BINARY)
        .args(["install", app_id, product, &device.os_version])
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
