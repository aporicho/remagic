use std::fs;
use std::path::Path;
use std::time::Duration;
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
        self.acquire_wakelock()?;
        self.mask_xochitl().await?;
        self.stop_if_loaded("paperweight.service").await?;
        self.stop("xochitl.service").await?;
        self.wait_inactive("xochitl.service").await?;
        self.wait_inactive("paperweight.service").await?;
        let _ = fs::remove_file("/tmp/epframebuffer.lock");
        Ok(())
    }

    pub async fn restore_system(&self) -> Result<(), String> {
        // The runtime unit becoming inactive is the display teardown fence.
        // Do not manufacture ownership by deleting another process's lock;
        // the runtime's graceful Qt teardown removes its own lock before this
        // method is reached.
        self.unmask_xochitl().await?;
        self.reset_failed().await?;
        self.start("xochitl.service").await?;
        self.start_paperweight_if_installed().await?;
        self.wait_active("xochitl.service").await?;
        self.release_wakelock()?;
        Ok(())
    }

    pub async fn start(&self, unit: &str) -> Result<(), String> {
        run("systemctl", &["start", unit]).await
    }

    pub async fn stop(&self, unit: &str) -> Result<(), String> {
        run("systemctl", &["stop", unit]).await
    }

    pub async fn stop_and_wait(&self, unit: &str) -> Result<(), String> {
        let first_error = self.stop(unit).await.err();
        if !self.is_active(unit).await {
            return Ok(());
        }

        // A stuck Qt display thread must be gone before xochitl is allowed to
        // reclaim the panel.  Keep this confined to the target systemd
        // cgroup, then ask systemd to settle the unit state once more.
        let _ = run(
            "systemctl",
            &["kill", "--kill-who=all", "--signal=KILL", unit],
        )
        .await;
        let _ = self.stop(unit).await;
        self.wait_inactive(unit).await.map_err(|wait_error| {
            if let Some(first_error) = first_error {
                format!("{first_error}; forced stop also failed: {wait_error}")
            } else {
                wait_error
            }
        })
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

    pub fn acquire_wakelock(&self) -> Result<(), String> {
        fs::write("/sys/power/wake_lock", WAKELOCK)
            .map_err(|error| format!("cannot acquire managed wake lock: {error}"))
    }

    pub fn release_wakelock(&self) -> Result<(), String> {
        fs::write("/sys/power/wake_unlock", WAKELOCK)
            .map_err(|error| format!("cannot release managed wake lock: {error}"))
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

    async fn stop_if_loaded(&self, unit: &str) -> Result<(), String> {
        let output = Command::new("systemctl")
            .args(["show", "--property=LoadState", "--value", unit])
            .output()
            .await
            .map_err(|error| format!("cannot inspect {unit}: {error}"))?;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "loaded" {
            self.stop(unit).await?;
        }
        Ok(())
    }

    async fn wait_active(&self, unit: &str) -> Result<(), String> {
        self.wait_for_state(unit, true).await
    }

    async fn wait_inactive(&self, unit: &str) -> Result<(), String> {
        self.wait_for_state(unit, false).await
    }

    async fn wait_for_state(&self, unit: &str, active: bool) -> Result<(), String> {
        for _ in 0..50 {
            if self.is_active(unit).await == active {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(format!(
            "{unit} did not become {} within 5 seconds",
            if active { "active" } else { "inactive" }
        ))
    }

    pub async fn start_transient_worker(
        &self,
        name: &str,
        arguments: &[String],
    ) -> Result<(), String> {
        let worker = "/home/root/apps/remagic/bin/remagic-vellum-worker";
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
    let output = tokio::time::timeout(
        Duration::from_secs(12),
        Command::new(program).args(arguments).output(),
    )
    .await
    .map_err(|_| format!("{program} {} timed out", arguments.join(" ")))?
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
