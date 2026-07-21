use std::fs;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

const WAKELOCK: &[u8] = b"remagic-managed\n";
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
        self.start_paperweight_if_installed().await
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
        self.start_paperweight_if_installed().await?;
        self.wait_active("xochitl.service").await?;
        self.release_wakelock()?;
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

fn parse_active_state(
    status_code: Option<i32>,
    stdout: &str,
    stderr: &str,
    unit: &str,
) -> Result<bool, String> {
    let states: Vec<_> = stdout
        .lines()
        .map(str::trim)
        .filter(|state| !state.is_empty())
        .collect();
    if states.is_empty() {
        // systemctl uses 3 for inactive and 4 for unknown/no units matching a
        // wildcard. Those both prove there is no active cgroup; transport and
        // D-Bus failures use other statuses and remain fail-closed.
        return match status_code {
            Some(3 | 4) => Ok(false),
            _ => Err(format!(
                "systemctl could not determine whether {unit} is active: {}",
                stderr.trim()
            )),
        };
    }
    let mut active = false;
    for state in states {
        match state {
            "active" | "activating" | "reloading" | "deactivating" => active = true,
            "inactive" | "failed" | "unknown" => {}
            other => {
                return Err(format!(
                    "systemctl returned unexpected state {other:?} for {unit}"
                ));
            }
        }
    }
    Ok(active)
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

async fn run(program: &str, arguments: &[&str]) -> Result<(), String> {
    let output = command_output(program, arguments, Duration::from_secs(12)).await?;
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

async fn command_output(
    program: &str,
    arguments: &[&str],
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let mut command = Command::new(program);
    command.args(arguments).kill_on_drop(true);
    tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| format!("{program} {} timed out", arguments.join(" ")))?
        .map_err(|error| format!("cannot execute {program}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_matching_template_instances_are_inactive() {
        assert_eq!(
            parse_active_state(Some(4), "", "", "remagic-app@*.service"),
            Ok(false)
        );
    }

    #[test]
    fn systemd_transport_failure_is_not_inactivity_evidence() {
        assert!(parse_active_state(
            Some(1),
            "",
            "Failed to connect to bus",
            "remagic-display-host.service"
        )
        .is_err());
    }

    #[test]
    fn transitional_states_still_own_the_unit() {
        for state in ["active", "activating", "reloading", "deactivating"] {
            assert_eq!(
                parse_active_state(Some(0), state, "", "owner.service"),
                Ok(true)
            );
        }
    }
}
