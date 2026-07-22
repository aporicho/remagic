use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;
use tracing::warn;

mod background;
mod freezer;
mod systemd;

pub use background::managed_background_unit;
use systemd::{command_output, parse_active_state, run};

const WAKELOCK: &[u8] = b"remagic-managed\n";
const WAKELOCK_NAME: &str = "remagic-managed";
const AUTOSLEEP: &str = "/sys/power/autosleep";
const SUSPEND_SUCCESS: &str = "/sys/power/suspend_stats/success";
const SUSPEND_START_TIMEOUT: Duration = Duration::from_secs(5);
const SUSPEND_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MANAGED_DOMAIN_MARKER: &str = "/run/remagic/managed-domain";
const MANAGED_DOMAIN_UNITS: [&str; 4] = [
    "remagic-home.service",
    "remagic-runtime.service",
    "remagic-app@*.service",
    "remagic-display-host.service",
];

#[derive(Clone, Debug, Default)]
pub struct SystemController;

impl SystemController {
    pub fn new() -> Self {
        Self
    }

    pub async fn ensure_system(&self) -> Result<(), String> {
        // A restarted supervisor always begins from the stock domain. Evict
        // every alternative owner and prove it inactive before making xochitl
        // authoritative. A failed stop must keep startup fail-closed.
        self.stop_managed_domain().await?;
        remove_managed_domain_marker()?;
        self.unmask_xochitl().await?;
        self.reset_failed().await?;
        self.start("xochitl.service").await?;
        self.wait_active("xochitl.service").await?;
        // A userspace wakelock is named kernel state rather than an
        // fd-scoped lease.  If remagicd was killed while owning the managed
        // domain, the name can survive the daemon process.  Only release it
        // after every managed display owner is gone and stock is active.
        self.release_wakelock_best_effort("startup recovery");
        self.start_paperweight_best_effort("startup recovery").await;
        Ok(())
    }

    pub async fn enter_managed(&self) -> Result<(), String> {
        self.acquire_wakelock()?;
        self.stop_managed_domain().await?;
        self.mask_xochitl().await?;
        self.stop_and_wait("paperweight.service").await?;
        self.stop_and_wait("xochitl.service").await?;
        let _ = fs::remove_file("/tmp/epframebuffer.lock");
        fs::write(MANAGED_DOMAIN_MARKER, b"v2\n")
            .map_err(|error| format!("cannot publish managed-domain marker: {error}"))?;
        Ok(())
    }

    pub async fn restore_system(&self) -> Result<(), String> {
        // The runtime unit becoming inactive is the display teardown fence.
        // Do not manufacture ownership by deleting another process's lock;
        // the runtime's graceful Qt teardown removes its own lock before this
        // method is reached.
        remove_managed_domain_marker()?;
        self.unmask_xochitl().await?;
        self.reset_failed().await?;
        self.start("xochitl.service").await?;
        self.wait_active("xochitl.service").await?;
        // xochitl is already authoritative at this point.  A wake-unlock
        // permission or device error must be reported, but must not strand
        // the power key inside ReMagic after stock has taken over.
        self.release_wakelock_best_effort("stock-domain restore");
        self.start_paperweight_best_effort("stock-domain restore")
            .await;
        Ok(())
    }

    pub async fn start(&self, unit: &str) -> Result<(), String> {
        run("systemctl", &["start", unit]).await
    }

    pub async fn start_and_wait(&self, unit: &str) -> Result<(), String> {
        self.start(unit).await?;
        self.wait_active(unit).await
    }

    pub async fn stop(&self, unit: &str) -> Result<(), String> {
        run("systemctl", &["stop", unit]).await
    }

    pub async fn stop_and_wait(&self, unit: &str) -> Result<(), String> {
        let first_error = self.stop(unit).await.err();
        if !self.is_active_checked(unit).await? {
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
        // Callers which only need a conservative hint must not interpret a
        // broken systemd query as proof that an owner disappeared.
        self.is_active_checked(unit).await.unwrap_or(true)
    }

    pub async fn suspend(&self) -> Result<(), String> {
        let autosleep = fs::read_to_string(AUTOSLEEP)
            .map_err(|error| format!("cannot inspect kernel autosleep mode: {error}"))?;
        if autosleep.trim() != "mem" {
            return Err(format!(
                "kernel autosleep mode is {:?}, expected mem",
                autosleep.trim()
            ));
        }
        let active = fs::read_to_string("/sys/power/wake_lock")
            .map_err(|error| format!("cannot inspect active wake locks: {error}"))?;
        let blockers = external_wake_locks(&active);
        if !blockers.is_empty() {
            return Err(format!(
                "kernel suspend is blocked by active wake locks: {}",
                blockers.join(" ")
            ));
        }

        // This device already runs the kernel autosleep worker. Writing
        // /sys/power/state after releasing the final wakelock races that
        // worker and is rejected with EBUSY. Capture a durable witness while
        // our lock still prevents sleep, release it, then wait until the
        // process resumes and the kernel success counter has advanced.
        let baseline = read_suspend_success()?;
        self.release_wakelock()?;
        let wait_for_resume = async {
            loop {
                let current = read_suspend_success()?;
                if current > baseline {
                    return Ok(());
                }
                tokio::time::sleep(SUSPEND_POLL_INTERVAL).await;
            }
        };
        tokio::time::timeout(SUSPEND_START_TIMEOUT, wait_for_resume)
            .await
            .map_err(|_| {
                let active = fs::read_to_string("/sys/power/wake_lock")
                    .unwrap_or_else(|_| "unavailable".into());
                format!(
                    "kernel autosleep did not complete within {} ms; active wake locks: {}",
                    SUSPEND_START_TIMEOUT.as_millis(),
                    active.trim()
                )
            })?
    }

    pub fn acquire_wakelock(&self) -> Result<(), String> {
        fs::write("/sys/power/wake_lock", WAKELOCK)
            .map_err(|error| format!("cannot acquire managed wake lock: {error}"))
    }

    pub fn release_wakelock(&self) -> Result<(), String> {
        interpret_wake_unlock(fs::write("/sys/power/wake_unlock", WAKELOCK))
    }

    fn release_wakelock_best_effort(&self, context: &str) {
        if let Err(error) = self.release_wakelock() {
            warn!(%error, %context, "managed wake lock cleanup failed");
        }
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
        let output = command_output(
            "systemctl",
            &[
                "show",
                "--property=LoadState",
                "--value",
                "paperweight.service",
            ],
            Duration::from_secs(5),
        )
        .await?;
        if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "loaded" {
            self.start("paperweight.service").await?;
        }
        Ok(())
    }

    async fn start_paperweight_best_effort(&self, context: &str) {
        if let Err(error) = self.start_paperweight_if_installed().await {
            // Paperweight augments the stock shell but does not own its
            // display or power-key safety.  Once xochitl is proven active,
            // an add-on failure must not trap the device in a ReMagic restore
            // loop or keep a stale wakelock alive.
            warn!(%error, %context, "Paperweight could not be started after stock recovery");
        }
    }

    async fn stop_managed_domain(&self) -> Result<(), String> {
        // Display host is last: clients may tear down first, but xochitl is
        // never started until the sole panel owner has been fenced out.
        for unit in MANAGED_DOMAIN_UNITS {
            self.stop_and_wait(unit)
                .await
                .map_err(|error| format!("cannot evict managed unit {unit}: {error}"))?;
        }
        Ok(())
    }

    pub async fn is_active_checked(&self, unit: &str) -> Result<bool, String> {
        let output =
            command_output("systemctl", &["is-active", unit], Duration::from_secs(5)).await?;
        parse_active_state(
            output.status.code(),
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
            unit,
        )
    }

    async fn wait_active(&self, unit: &str) -> Result<(), String> {
        self.wait_for_state(unit, true).await
    }

    async fn wait_inactive(&self, unit: &str) -> Result<(), String> {
        self.wait_for_state(unit, false).await
    }

    async fn wait_for_state(&self, unit: &str, active: bool) -> Result<(), String> {
        let wait = async {
            loop {
                if self.is_active_checked(unit).await? == active {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        tokio::time::timeout(Duration::from_secs(5), wait)
            .await
            .map_err(|_| {
                format!(
                    "{unit} did not become {} within 5 seconds",
                    if active { "active" } else { "inactive" }
                )
            })?
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

fn read_suspend_success() -> Result<u64, String> {
    let value = fs::read_to_string(SUSPEND_SUCCESS)
        .map_err(|error| format!("cannot inspect kernel suspend counter: {error}"))?;
    parse_suspend_success(&value)
}

fn parse_suspend_success(value: &str) -> Result<u64, String> {
    value
        .trim()
        .parse::<u64>()
        .map_err(|error| format!("invalid kernel suspend counter {:?}: {error}", value.trim()))
}

fn external_wake_locks(value: &str) -> Vec<&str> {
    value
        .split_whitespace()
        .filter(|name| *name != WAKELOCK_NAME)
        .collect()
}

fn interpret_wake_unlock(result: io::Result<()>) -> Result<(), String> {
    match result {
        Ok(()) => Ok(()),
        // The kernel's /sys/power/wake_unlock interface reports EINVAL when
        // this name is absent.  Releasing an already-released named lock is
        // therefore a successful idempotent cleanup for our state machine.
        Err(error) if error.raw_os_error() == Some(libc::EINVAL) => Ok(()),
        Err(error) => Err(format!("cannot release managed wake lock: {error}")),
    }
}

fn remove_managed_domain_marker() -> Result<(), String> {
    match fs::remove_file(MANAGED_DOMAIN_MARKER) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "cannot remove managed-domain marker {MANAGED_DOMAIN_MARKER}: {error}"
        )),
    }
}

#[cfg(test)]
mod tests;
