use std::fs;
use std::path::Path;
use tokio::process::Command;

const WAKELOCK: &[u8] = b"remagic-managed\n";

#[derive(Clone, Debug, Default)]
pub struct SystemController;

impl SystemController {
    pub fn new() -> Self {
        Self
    }

    pub async fn ensure_system(&self) -> Result<(), String> {
        self.unmask_xochitl().await?;
        self.reset_failed().await?;
        self.start("xochitl.service").await?;
        self.start_paperweight_if_installed().await
    }

    pub async fn enter_managed(&self) -> Result<(), String> {
        self.acquire_wakelock();
        self.stop("xochitl.service").await?;
        self.mask_xochitl().await?;
        let _ = fs::remove_file("/tmp/epframebuffer.lock");
        Ok(())
    }

    pub async fn restore_system(&self) -> Result<(), String> {
        let _ = fs::remove_file("/tmp/epframebuffer.lock");
        self.unmask_xochitl().await?;
        self.reset_failed().await?;
        self.start("xochitl.service").await?;
        self.start_paperweight_if_installed().await?;
        self.release_wakelock();
        Ok(())
    }

    pub async fn start(&self, unit: &str) -> Result<(), String> {
        run("systemctl", &["start", unit]).await
    }

    pub async fn stop(&self, unit: &str) -> Result<(), String> {
        run("systemctl", &["stop", unit]).await
    }

    pub async fn restart(&self, unit: &str) -> Result<(), String> {
        run("systemctl", &["restart", unit]).await
    }

    pub async fn is_active(&self, unit: &str) -> bool {
        Command::new("systemctl")
            .args(["is-active", "--quiet", unit])
            .status()
            .await
            .is_ok_and(|status| status.success())
    }

    pub async fn suspend(&self) -> Result<(), String> {
        fs::write("/sys/power/state", b"mem\n").map_err(|e| format!("suspend failed: {e}"))
    }

    pub fn acquire_wakelock(&self) {
        let _ = fs::write("/sys/power/wake_lock", WAKELOCK);
    }

    pub fn release_wakelock(&self) {
        let _ = fs::write("/sys/power/wake_unlock", WAKELOCK);
    }

    async fn reset_failed(&self) -> Result<(), String> {
        run(
            "systemctl",
            &["reset-failed", "xochitl.service", "paperweight.service"],
        )
        .await
    }

    async fn mask_xochitl(&self) -> Result<(), String> {
        run("systemctl", &["mask", "--runtime", "xochitl.service"]).await
    }

    async fn unmask_xochitl(&self) -> Result<(), String> {
        run("systemctl", &["unmask", "--runtime", "xochitl.service"]).await
    }

    async fn start_paperweight_if_installed(&self) -> Result<(), String> {
        let output = Command::new("systemctl")
            .args([
                "show",
                "--property=LoadState",
                "--value",
                "paperweight.service",
            ])
            .output()
            .await
            .map_err(|e| format!("cannot inspect paperweight.service: {e}"))?;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "loaded" {
            self.start("paperweight.service").await?;
        }
        Ok(())
    }

    pub async fn start_transient_worker(
        &self,
        name: &str,
        arguments: &[String],
    ) -> Result<(), String> {
        let worker = "/home/root/apps/remagic/remagic-vellum-worker";
        if !Path::new(worker).exists() {
            return Err("Vellum worker is not installed".into());
        }
        let mut args = vec![
            "--unit".to_string(),
            format!("{name}-{}", std::process::id()),
            "--collect".to_string(),
            worker.to_string(),
        ];
        args.extend(arguments.iter().cloned());
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run("systemd-run", &refs).await
    }
}

async fn run(program: &str, arguments: &[&str]) -> Result<(), String> {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .await
        .map_err(|e| format!("cannot execute {program}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} {} failed: {}",
            program,
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}
