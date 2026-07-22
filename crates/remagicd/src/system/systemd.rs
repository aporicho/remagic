use std::time::Duration;
use tokio::process::Command;

pub(super) async fn run(program: &str, arguments: &[&str]) -> Result<(), String> {
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

pub(super) async fn command_output(
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

pub(super) fn parse_active_state(
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
