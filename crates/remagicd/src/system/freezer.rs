use super::{command_output, SystemController};
use std::fs;
use std::time::Duration;

const TRANSITION_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

impl SystemController {
    /// Freeze every process in a systemd service cgroup and wait until
    /// systemd reports the stable `frozen` state. `is-active` intentionally
    /// remains true for a frozen service, so callers must not use it as the
    /// completion fence.
    pub async fn freeze_and_wait(&self, unit: &str) -> Result<(), String> {
        if !self.is_active_checked(unit).await? {
            return Err(format!("cannot freeze inactive unit {unit}"));
        }
        match freeze_action(self.freezer_state(unit).await?) {
            FreezeAction::AlreadyFrozen => return Ok(()),
            FreezeAction::WaitForFrozen => {
                return self
                    .wait_freezer_state(unit, UnitFreezerState::Frozen)
                    .await;
            }
            FreezeAction::WaitForRunningThenFreeze => {
                self.wait_freezer_state(unit, UnitFreezerState::Running)
                    .await?;
            }
            FreezeAction::Freeze => {}
        }
        match systemctl(&["freeze", unit]).await {
            Ok(()) => {
                self.wait_freezer_state(unit, UnitFreezerState::Frozen)
                    .await
            }
            Err(error) if freeze_failure_action(&error) == FreezeFailureAction::SignalStop => {
                self.signal_and_wait(unit, "SIGSTOP", ProcessExecutionState::Stopped)
                    .await
                    .map_err(|fallback| {
                        format!("native freezer is unsupported ({error}); SIGSTOP fallback failed: {fallback}")
                    })
            }
            Err(error) => Err(error),
        }
    }

    /// Resume a systemd service cgroup and prove it is schedulable before a
    /// lifecycle command is delivered to the application.
    pub async fn thaw_and_wait(&self, unit: &str) -> Result<(), String> {
        if !self.is_active_checked(unit).await? {
            return Err(format!("cannot thaw inactive unit {unit}"));
        }
        match thaw_action(self.freezer_state(unit).await?) {
            ThawAction::SignalContinue => {
                return self
                    .signal_and_wait(unit, "SIGCONT", ProcessExecutionState::Running)
                    .await;
            }
            ThawAction::WaitForNative => {
                return self
                    .wait_freezer_state(unit, UnitFreezerState::Running)
                    .await;
            }
            ThawAction::Native => {}
        }
        systemctl(&["thaw", unit]).await?;
        self.wait_freezer_state(unit, UnitFreezerState::Running)
            .await
    }

    async fn signal_and_wait(
        &self,
        unit: &str,
        signal: &str,
        expected: ProcessExecutionState,
    ) -> Result<(), String> {
        let pid = self.main_pid(unit).await?;
        systemctl(&[
            "kill",
            "--kill-whom=all",
            &format!("--signal={signal}"),
            unit,
        ])
        .await?;
        wait_process_state(pid, expected).await
    }

    async fn main_pid(&self, unit: &str) -> Result<u32, String> {
        let output = command_output(
            "systemctl",
            &["show", "--property=MainPID", "--value", unit],
            TRANSITION_TIMEOUT,
        )
        .await?;
        if !output.status.success() {
            return Err(format!(
                "systemctl could not inspect MainPID for {unit}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        parse_main_pid(&String::from_utf8_lossy(&output.stdout), unit)
    }

    async fn wait_freezer_state(
        &self,
        unit: &str,
        expected: UnitFreezerState,
    ) -> Result<(), String> {
        let wait = async {
            loop {
                let state = self.freezer_state(unit).await?;
                if state == expected {
                    return Ok(());
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        };
        tokio::time::timeout(TRANSITION_TIMEOUT, wait)
            .await
            .map_err(|_| {
                format!(
                    "{unit} did not become {} within {} ms",
                    expected.as_str(),
                    TRANSITION_TIMEOUT.as_millis()
                )
            })?
    }

    async fn freezer_state(&self, unit: &str) -> Result<UnitFreezerState, String> {
        let output = command_output(
            "systemctl",
            &["show", "--property=FreezerState", "--value", unit],
            Duration::from_secs(5),
        )
        .await?;
        if !output.status.success() {
            return Err(format!(
                "systemctl could not inspect freezer state for {unit}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        parse_freezer_state(&String::from_utf8_lossy(&output.stdout), unit)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FreezeFailureAction {
    SignalStop,
    ReturnError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FreezeAction {
    AlreadyFrozen,
    WaitForFrozen,
    WaitForRunningThenFreeze,
    Freeze,
}

fn freeze_action(state: UnitFreezerState) -> FreezeAction {
    match state {
        UnitFreezerState::Frozen => FreezeAction::AlreadyFrozen,
        UnitFreezerState::Freezing => FreezeAction::WaitForFrozen,
        UnitFreezerState::Thawing => FreezeAction::WaitForRunningThenFreeze,
        UnitFreezerState::Running => FreezeAction::Freeze,
    }
}

fn freeze_failure_action(error: &str) -> FreezeFailureAction {
    let error = error.to_ascii_lowercase();
    if error.contains("does not support freezing") || error.contains("not supported") {
        FreezeFailureAction::SignalStop
    } else {
        FreezeFailureAction::ReturnError
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ThawAction {
    SignalContinue,
    WaitForNative,
    Native,
}

fn thaw_action(state: UnitFreezerState) -> ThawAction {
    match state {
        UnitFreezerState::Running => ThawAction::SignalContinue,
        UnitFreezerState::Thawing => ThawAction::WaitForNative,
        UnitFreezerState::Frozen | UnitFreezerState::Freezing => ThawAction::Native,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnitFreezerState {
    Running,
    Freezing,
    Frozen,
    Thawing,
}

impl UnitFreezerState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Freezing => "freezing",
            Self::Frozen => "frozen",
            Self::Thawing => "thawing",
        }
    }
}

fn parse_freezer_state(value: &str, unit: &str) -> Result<UnitFreezerState, String> {
    match value.trim() {
        "running" => Ok(UnitFreezerState::Running),
        "freezing" => Ok(UnitFreezerState::Freezing),
        "frozen" => Ok(UnitFreezerState::Frozen),
        "thawing" => Ok(UnitFreezerState::Thawing),
        other => Err(format!(
            "systemctl returned unexpected freezer state {other:?} for {unit}"
        )),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessExecutionState {
    Running,
    Stopped,
}

fn parse_main_pid(value: &str, unit: &str) -> Result<u32, String> {
    let pid = value
        .trim()
        .parse::<u32>()
        .map_err(|error| format!("invalid MainPID for {unit}: {error}"))?;
    if pid == 0 {
        Err(format!("active unit {unit} has no MainPID"))
    } else {
        Ok(pid)
    }
}

fn parse_process_state(status: &str, pid: u32) -> Result<ProcessExecutionState, String> {
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix("State:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.chars().next())
        .ok_or_else(|| format!("/proc/{pid}/status has no process State"))?;
    Ok(if matches!(value, 'T' | 't') {
        ProcessExecutionState::Stopped
    } else {
        ProcessExecutionState::Running
    })
}

async fn wait_process_state(pid: u32, expected: ProcessExecutionState) -> Result<(), String> {
    let wait = async {
        loop {
            let path = format!("/proc/{pid}/status");
            let status = fs::read_to_string(&path)
                .map_err(|error| format!("cannot inspect {path}: {error}"))?;
            if parse_process_state(&status, pid)? == expected {
                return Ok(());
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    };
    tokio::time::timeout(TRANSITION_TIMEOUT, wait)
        .await
        .map_err(|_| {
            format!(
                "process {pid} did not become {} within {} ms",
                expected.as_str(),
                TRANSITION_TIMEOUT.as_millis()
            )
        })?
}

impl ProcessExecutionState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
        }
    }
}

async fn systemctl(arguments: &[&str]) -> Result<(), String> {
    let output = command_output("systemctl", arguments, TRANSITION_TIMEOUT).await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_255_freezer_states_are_parsed_strictly() {
        assert_eq!(
            parse_freezer_state("frozen\n", "reader.service"),
            Ok(UnitFreezerState::Frozen)
        );
        assert_eq!(
            parse_freezer_state("thawing", "reader.service"),
            Ok(UnitFreezerState::Thawing)
        );
        assert!(parse_freezer_state("", "reader.service").is_err());
        assert!(parse_freezer_state("paused", "reader.service").is_err());
    }

    #[test]
    fn only_explicit_unsupported_errors_enable_sigstop_fallback() {
        for error in [
            "Unit remagic-app@koreader.service does not support freezing",
            "Failed to freeze unit: Operation not supported",
        ] {
            assert_eq!(
                freeze_failure_action(error),
                FreezeFailureAction::SignalStop
            );
        }
        for error in [
            "Access denied",
            "Failed to connect to bus",
            "Unit entered failed state",
        ] {
            assert_eq!(
                freeze_failure_action(error),
                FreezeFailureAction::ReturnError
            );
        }
    }

    #[test]
    fn freeze_waits_for_an_opposite_thaw_transition_before_restarting() {
        assert_eq!(
            freeze_action(UnitFreezerState::Frozen),
            FreezeAction::AlreadyFrozen
        );
        assert_eq!(
            freeze_action(UnitFreezerState::Freezing),
            FreezeAction::WaitForFrozen
        );
        assert_eq!(
            freeze_action(UnitFreezerState::Thawing),
            FreezeAction::WaitForRunningThenFreeze
        );
        assert_eq!(
            freeze_action(UnitFreezerState::Running),
            FreezeAction::Freeze
        );
    }

    #[test]
    fn thaw_backend_is_selected_from_systemd_freezer_state() {
        assert_eq!(
            thaw_action(UnitFreezerState::Running),
            ThawAction::SignalContinue
        );
        assert_eq!(
            thaw_action(UnitFreezerState::Thawing),
            ThawAction::WaitForNative
        );
        assert_eq!(thaw_action(UnitFreezerState::Frozen), ThawAction::Native);
    }

    #[test]
    fn proc_status_and_main_pid_are_parsed_strictly() {
        assert_eq!(parse_main_pid("42\n", "reader.service"), Ok(42));
        assert!(parse_main_pid("0", "reader.service").is_err());
        assert!(parse_main_pid("pid 42", "reader.service").is_err());
        assert_eq!(
            parse_process_state("Name:\treader\nState:\tT (stopped)\n", 42),
            Ok(ProcessExecutionState::Stopped)
        );
        assert_eq!(
            parse_process_state("State:\tt (tracing stop)\n", 42),
            Ok(ProcessExecutionState::Stopped)
        );
        assert_eq!(
            parse_process_state("State:\tS (sleeping)\n", 42),
            Ok(ProcessExecutionState::Running)
        );
        assert!(parse_process_state("Name:\treader\n", 42).is_err());
    }
}
