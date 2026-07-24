use crate::daemon::{Daemon, Event};
use remagic_core::AppId;
use remagic_protocol::{
    RuntimeAppCommand, RuntimeAppReply, RuntimeAppRequest, RUNTIME_APP_PROTOCOL_V1,
    RUNTIME_APP_PROTOCOL_V2,
};
use std::fs;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub(super) async fn serve(
    mut stream: UnixStream,
    daemon: Arc<Daemon>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        stream
            .write_all(b"{\"ok\":false,\"error\":\"permission denied\"}\n")
            .await?;
        return Ok(());
    }
    let peer_app = runtime_peer_app(&stream);
    let mut line = String::new();
    let count = BufReader::new(&mut stream).read_line(&mut line).await?;
    let reply = parse(count, &line, &daemon, peer_app).await;
    stream.write_all(&serde_json::to_vec(&reply)?).await?;
    stream.write_all(b"\n").await?;
    Ok(())
}

async fn parse(
    count: usize,
    line: &str,
    daemon: &Daemon,
    peer_app: Option<AppId>,
) -> RuntimeAppReply {
    let request_id_hint = runtime_request_id(line);
    if count == 0 || !line.ends_with('\n') || line.len() > 64 * 1024 {
        return RuntimeAppReply::error(request_id_hint, "incomplete application request");
    }
    let request = match serde_json::from_str::<RuntimeAppRequest>(line) {
        Ok(request) => request,
        Err(error) => {
            return RuntimeAppReply::error(
                request_id_hint,
                format!("invalid application request: {error}"),
            )
        }
    };
    if request.request_id.is_empty() {
        return RuntimeAppReply::error(None, "unsupported application request");
    }
    match request.command {
        RuntimeAppCommand::OpenApp { app, open_path } => {
            open_app(daemon, request.version, request.request_id, app, open_path).await
        }
        RuntimeAppCommand::SetInputMode { token, mode } => {
            set_input_mode(daemon, request.version, request.request_id, token, mode).await
        }
        RuntimeAppCommand::BeginWork {
            class,
            reason,
            requested_ms,
        } => {
            begin_work(
                daemon,
                request.version,
                request.request_id,
                peer_app,
                class,
                reason,
                requested_ms,
            )
            .await
        }
        RuntimeAppCommand::FinishWork {
            lease_id,
            visible_result,
        } => {
            finish_work(
                daemon,
                request.version,
                request.request_id,
                peer_app,
                lease_id,
                visible_result,
            )
            .await
        }
    }
}

async fn open_app(
    daemon: &Daemon,
    version: u8,
    request_id: String,
    app: AppId,
    open_path: Option<std::path::PathBuf>,
) -> RuntimeAppReply {
    if version != RUNTIME_APP_PROTOCOL_V1 {
        return RuntimeAppReply::error(
            Some(request_id),
            "open_app requires runtime protocol version 1",
        );
    }
    match daemon.enqueue(Event::Launch(app, open_path)).await {
        remagic_protocol::Response::Ok => RuntimeAppReply::open_accepted(),
        remagic_protocol::Response::Error { message } => {
            RuntimeAppReply::error(Some(request_id), message)
        }
        _ => RuntimeAppReply::error(Some(request_id), "unexpected manager reply"),
    }
}

async fn set_input_mode(
    daemon: &Daemon,
    version: u8,
    request_id: String,
    token: remagic_core::AppToken,
    mode: remagic_protocol::InputMode,
) -> RuntimeAppReply {
    if version != RUNTIME_APP_PROTOCOL_V2 {
        return RuntimeAppReply::error(
            Some(request_id),
            "set_input_mode requires runtime protocol version 2",
        );
    }
    match daemon.set_input_mode(&token, mode).await {
        Ok(ink_enabled) => {
            RuntimeAppReply::input_mode_accepted(request_id, token, mode, ink_enabled)
        }
        Err(error) => RuntimeAppReply::error(Some(request_id), error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn begin_work(
    daemon: &Daemon,
    version: u8,
    request_id: String,
    peer_app: Option<AppId>,
    class: remagic_core::WorkClass,
    reason: String,
    requested_ms: u64,
) -> RuntimeAppReply {
    if version != RUNTIME_APP_PROTOCOL_V2 || requested_ms == 0 {
        return RuntimeAppReply::error(Some(request_id), "invalid begin_work request");
    }
    let Some(app_id) = peer_app else {
        return RuntimeAppReply::error(
            Some(request_id),
            "work leases require a managed application cgroup",
        );
    };
    if reason.trim().is_empty() || reason.len() > 256 {
        return RuntimeAppReply::error(Some(request_id), "invalid work reason");
    }
    let lease = daemon
        .power
        .begin_work(app_id, class, reason, requested_ms)
        .await;
    RuntimeAppReply::work_accepted(request_id, lease)
}

async fn finish_work(
    daemon: &Daemon,
    version: u8,
    request_id: String,
    peer_app: Option<AppId>,
    lease_id: u64,
    visible_result: bool,
) -> RuntimeAppReply {
    if version != RUNTIME_APP_PROTOCOL_V2 || lease_id == 0 {
        return RuntimeAppReply::error(Some(request_id), "invalid finish_work request");
    }
    let Some(app_id) = peer_app else {
        return RuntimeAppReply::error(
            Some(request_id),
            "work leases require a managed application cgroup",
        );
    };
    if daemon
        .power
        .finish_work(&app_id, lease_id, visible_result)
        .await
    {
        RuntimeAppReply::work_finished(request_id)
    } else {
        RuntimeAppReply::error(Some(request_id), "work lease is absent or not owned")
    }
}

fn runtime_peer_app(stream: &UnixStream) -> Option<AppId> {
    let pid = stream.peer_cred().ok()?.pid()?;
    let cgroup = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    app_id_from_cgroup(&cgroup)
}

fn app_id_from_cgroup(cgroup: &str) -> Option<AppId> {
    cgroup
        .lines()
        .flat_map(|line| line.split('/'))
        .find_map(|part| {
            let id = part
                .strip_prefix("remagic-app@")
                .and_then(|value| value.strip_suffix(".service"))
                .or_else(|| {
                    part.strip_prefix("remagic-background-")
                        .and_then(|value| value.strip_suffix(".service"))
                })?;
            AppId::new(id.to_owned()).ok()
        })
}

fn runtime_request_id(line: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()?
        .get("request_id")?
        .as_str()
        .filter(|request_id| !request_id.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_identity_is_recovered_from_a_structurally_invalid_request() {
        let line = r#"{"version":1,"request_id":"mode-bad","command":"set_input_mode","app":"magicpaper","mode":"unknown"}"#;
        assert_eq!(runtime_request_id(line).as_deref(), Some("mode-bad"));
        let error = serde_json::from_str::<RuntimeAppRequest>(line).unwrap_err();
        let reply = RuntimeAppReply::error(runtime_request_id(line), error.to_string());
        assert_eq!(reply.request_id.as_deref(), Some("mode-bad"));
        assert!(!reply.ok);
    }

    #[test]
    fn empty_or_non_string_request_identity_is_not_echoed() {
        for line in [
            r#"{"request_id":""}"#,
            r#"{"request_id":17}"#,
            r#"{"version":1}"#,
            "not-json",
        ] {
            assert_eq!(runtime_request_id(line), None);
        }
    }

    #[test]
    fn work_lease_identity_is_derived_only_from_managed_application_units() {
        assert_eq!(
            app_id_from_cgroup("0::/system.slice/remagic-app@magicpaper.service")
                .unwrap()
                .as_str(),
            "magicpaper"
        );
        assert_eq!(
            app_id_from_cgroup("0::/system.slice/remagic-background-magicpaper.service")
                .unwrap()
                .as_str(),
            "magicpaper"
        );
        assert!(app_id_from_cgroup("0::/system.slice/ssh.service").is_none());
        assert!(app_id_from_cgroup(
            "0::/system.slice/remagic-background-magicpaper.service.attacker"
        )
        .is_none());
    }
}
