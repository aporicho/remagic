use crate::bootstrap::PreparedApplication;
use crate::lifecycle_bridge::LifecycleBridge;
use remagic_core::ShutdownPolicy;
use remagic_protocol::ShutdownReason;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::time::Instant;

pub(crate) struct RunningApplication {
    pub prepared: PreparedApplication,
    pub child: Child,
}

pub(crate) async fn launch(
    mut prepared: PreparedApplication,
) -> Result<RunningApplication, Box<dyn std::error::Error>> {
    let mut command = application_command(&prepared)?;
    let mut child = command.spawn()?;
    drop(prepared.lifecycle.child_descriptor.take());
    if let (Some(bridge), Some(environment)) = (
        &prepared.lifecycle.bridge,
        prepared.plan.launch_environment.clone(),
    ) {
        if let Err(error) = bridge
            .send_start(
                environment,
                prepared.resume_payload.clone(),
                prepared.open_path.clone(),
            )
            .await
        {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(error.into());
        }
    }
    Ok(RunningApplication { prepared, child })
}

fn application_command(prepared: &PreparedApplication) -> io::Result<Command> {
    let mut command = Command::new(&prepared.manifest.exec);
    command
        .args(&prepared.manifest.args)
        .current_dir(&prepared.manifest.working_dir);
    if prepared.plan.clear_inherited_environment {
        command.env_clear();
    }
    command.envs(&prepared.plan.variables);
    if let Some(path) = &prepared.open_path {
        command.arg(path);
    }
    if let Some(descriptor) = prepared.lifecycle.child_descriptor.as_ref() {
        let child_fd = descriptor.as_raw_fd();
        // Keep CLOEXEC in the supervisor and clear it only after fork.
        unsafe {
            command.pre_exec(move || clear_close_on_exec(child_fd));
        }
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    Ok(command)
}

pub(crate) async fn graceful_stop(
    child: &mut Child,
    bridge: Option<&LifecycleBridge>,
    policy: &ShutdownPolicy,
) -> io::Result<ExitStatus> {
    let started = Instant::now();
    let graceful_deadline = started + Duration::from_millis(policy.graceful_timeout_ms);
    match request_lifecycle_shutdown_until(child, bridge, graceful_deadline).await? {
        ShutdownRequest::ChildExited(status) => return Ok(status),
        ShutdownRequest::Delivered => {
            if let Some(status) = wait_until(child, started, policy.graceful_timeout_ms).await? {
                return Ok(status);
            }
        }
        ShutdownRequest::Unavailable => {}
    }
    signal_child(child, libc::SIGTERM)?;
    if let Some(status) = wait_until(child, started, policy.term_timeout_ms).await? {
        return Ok(status);
    }
    child.start_kill()?;
    if let Some(status) = wait_until(child, started, policy.kill_timeout_ms).await? {
        return Ok(status);
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "application did not exit within {} ms after lifecycle, TERM, and KILL",
            policy.kill_timeout_ms
        ),
    ))
}

enum ShutdownRequest {
    ChildExited(ExitStatus),
    Delivered,
    Unavailable,
}

async fn request_lifecycle_shutdown_until(
    child: &mut Child,
    bridge: Option<&LifecycleBridge>,
    deadline: Instant,
) -> io::Result<ShutdownRequest> {
    let Some(bridge) = bridge else {
        return Ok(ShutdownRequest::Unavailable);
    };
    tokio::select! {
        status = child.wait() => Ok(ShutdownRequest::ChildExited(status?)),
        delivery = tokio::time::timeout_at(
            deadline,
            bridge.request_shutdown(ShutdownReason::Upgrade),
        ) => {
            match delivery {
                Ok(Ok(())) => Ok(ShutdownRequest::Delivered),
                Ok(Err(error)) => {
                    eprintln!("remagic-runner: lifecycle shutdown delivery failed: {error}");
                    Ok(ShutdownRequest::Unavailable)
                }
                Err(_) => {
                    eprintln!("remagic-runner: lifecycle shutdown delivery reached its graceful deadline");
                    Ok(ShutdownRequest::Unavailable)
                }
            }
        }
    }
}

async fn wait_until(
    child: &mut Child,
    started: Instant,
    deadline_ms: u64,
) -> io::Result<Option<ExitStatus>> {
    let deadline = started + Duration::from_millis(deadline_ms);
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return Ok(None);
    };
    match tokio::time::timeout(remaining, child.wait()).await {
        Ok(status) => status.map(Some),
        Err(_) => Ok(None),
    }
}

fn signal_child(child: &Child, signal: i32) -> io::Result<()> {
    let Some(pid) = child.id() else {
        return Ok(());
    };
    if unsafe { libc::kill(pid as i32, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

pub(crate) fn clear_close_on_exec(descriptor: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
