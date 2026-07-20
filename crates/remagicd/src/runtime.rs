use remagic_core::AppId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub const SOCKET: &str = "/run/remagic/runtime-app.sock";
const MAX_REPLY: usize = 64 * 1024;
const APP_STOP_TIMEOUT: Duration = Duration::from_secs(7);
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Serialize)]
struct RuntimeRequest<'a> {
    version: u8,
    request_id: String,
    source: &'static str,
    command: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    app: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    open_path: Option<&'a Path>,
}

#[derive(Debug, Deserialize)]
struct RuntimeReply {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    status: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    active_app: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    qtfb_connected: bool,
    #[serde(default)]
    first_frame: bool,
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    foreground_epoch: u64,
    #[serde(default)]
    full_refresh_complete: bool,
    #[serde(default)]
    app_statuses: BTreeMap<String, RuntimeAppReply>,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct RuntimeAppReply {
    #[serde(default)]
    state: String,
    #[serde(default)]
    error: String,
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    foreground_epoch: u64,
    #[serde(default)]
    qtfb_connected: bool,
    #[serde(default)]
    first_frame: bool,
    #[serde(default)]
    full_refresh_complete: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeLaunch {
    pub generation: u64,
}

pub async fn open_app(app: &AppId, open_path: Option<&Path>) -> Result<RuntimeLaunch, String> {
    request("open_app", Some(app), open_path).await?;
    wait_app_ready(app).await
}

pub async fn show_manager() -> Result<(), String> {
    request("show_manager", None, None).await
}

pub async fn close_app(app: &AppId) -> Result<(), String> {
    request("close_app", Some(app), None).await?;
    wait_app_stopped(app).await
}

async fn request(
    command: &str,
    app: Option<&AppId>,
    open_path: Option<&Path>,
) -> Result<(), String> {
    request_at(SOCKET, command, app, open_path)
        .await
        .map(|_| ())
}

async fn request_at(
    socket: &str,
    command: &str,
    app: Option<&AppId>,
    open_path: Option<&Path>,
) -> Result<RuntimeReply, String> {
    let request_id = format!(
        "remagicd-{}-{}",
        std::process::id(),
        NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
    );
    let request = RuntimeRequest {
        version: 1,
        request_id,
        source: "daemon",
        command,
        app: app.map(AppId::as_str),
        open_path,
    };
    let mut body = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
    body.push(b'\n');

    let operation = async {
        let stream = UnixStream::connect(socket)
            .await
            .map_err(|error| format!("cannot connect to runtime: {error}"))?;
        exchange(stream, &body, command).await
    };

    tokio::time::timeout(Duration::from_secs(3), operation)
        .await
        .map_err(|_| format!("runtime command {command} timed out"))?
}

async fn exchange<S>(mut stream: S, body: &[u8], command: &str) -> Result<RuntimeReply, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream
        .write_all(&body)
        .await
        .map_err(|error| format!("cannot write runtime command: {error}"))?;
    stream
        .shutdown()
        .await
        .map_err(|error| format!("cannot finish runtime command: {error}"))?;

    let mut reply_line = String::new();
    let mut reader = BufReader::new(stream).take(MAX_REPLY as u64);
    let count = reader
        .read_line(&mut reply_line)
        .await
        .map_err(|error| format!("cannot read runtime reply: {error}"))?;
    if count == 0 || !reply_line.ends_with('\n') {
        return Err("runtime closed without a complete reply".into());
    }
    let reply: RuntimeReply = serde_json::from_str(&reply_line)
        .map_err(|error| format!("invalid runtime reply: {error}"))?;
    if reply.ok || matches!(reply.status.as_str(), "ok" | "accepted" | "ready") {
        Ok(reply)
    } else if reply.error.is_empty() {
        Err(format!("runtime rejected {command}"))
    } else {
        Err(reply.error)
    }
}

async fn wait_app_ready(app: &AppId) -> Result<RuntimeLaunch, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let mut observed_generation = None;
    loop {
        let reply = request_at(SOCKET, "status", None, None).await?;
        // A failed launch returns the UI to the manager, which clears
        // active_app.  Keep following the target's per-app snapshot so a
        // semantic-ready timeout or early process failure is reported
        // immediately instead of being misreported as a generic 15s timeout.
        if let Some(status) = reply.app_statuses.get(app.as_str()) {
            if observed_generation.is_none() && status.generation != 0 {
                observed_generation = Some(status.generation);
            }
            if status.generation != 0
                && observed_generation.is_some_and(|generation| generation != status.generation)
            {
                return Err(format!(
                    "runtime replaced {} while it was launching",
                    app.as_str()
                ));
            }
            if let Some(error) = terminal_launch_error(app, status) {
                return Err(error);
            }
        }
        if reply.active_app == app.as_str() {
            if observed_generation.is_none() && reply.generation != 0 {
                observed_generation = Some(reply.generation);
            }
            if observed_generation.is_some_and(|generation| generation != reply.generation) {
                return Err(format!(
                    "runtime replaced {} while it was launching",
                    app.as_str()
                ));
            }
            if matches!(
                reply.state.as_str(),
                "crashed" | "unavailable" | "error" | "exited"
            ) {
                return Err(if reply.error.is_empty() {
                    format!("runtime reported {} for {}", reply.state, app.as_str())
                } else {
                    format!("runtime could not launch {}: {}", app.as_str(), reply.error)
                });
            }
            if reply.state == "foreground"
                && reply.qtfb_connected
                && reply.first_frame
                && !reply.full_refresh_complete
                && !reply.error.is_empty()
            {
                return Err(format!(
                    "runtime could not present {}: {}",
                    app.as_str(),
                    reply.error
                ));
            }
            if reply.state == "foreground"
                && reply.generation != 0
                && reply.qtfb_connected
                && reply.first_frame
                && reply.full_refresh_complete
            {
                return Ok(RuntimeLaunch {
                    generation: reply.generation,
                });
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "{} did not connect QTFB and submit a first frame within 15 seconds",
                app.as_str()
            ));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn terminal_launch_error(app: &AppId, status: &RuntimeAppReply) -> Option<String> {
    if !matches!(
        status.state.as_str(),
        "crashed" | "unavailable" | "error" | "exited"
    ) {
        return None;
    }
    Some(if status.error.is_empty() {
        format!("runtime reported {} for {}", status.state, app.as_str())
    } else {
        format!(
            "runtime could not launch {}: {}",
            app.as_str(),
            status.error
        )
    })
}

async fn wait_app_stopped(app: &AppId) -> Result<(), String> {
    // KOReader's last-resort process-group KILL is issued at 5.5 seconds.
    // Leave enough time for QProcess::finished, QML window removal and the
    // resulting status reply to cross the runtime socket on a busy tablet.
    let deadline = tokio::time::Instant::now() + APP_STOP_TIMEOUT;
    let mut observed_generation = None;
    loop {
        let reply = request_at(SOCKET, "status", None, None).await?;
        match reply.app_statuses.get(app.as_str()) {
            None => return Ok(()),
            Some(status) => {
                observed_generation.get_or_insert(status.generation);
                if observed_generation.is_some_and(|generation| generation != status.generation) {
                    return Err(format!(
                        "runtime replaced {} while the previous generation was closing",
                        app.as_str()
                    ));
                }
                if matches!(
                    status.state.as_str(),
                    "exited" | "crashed" | "unavailable" | "error"
                ) {
                    return Ok(());
                }
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "runtime did not stop {} within {} seconds",
                app.as_str(),
                APP_STOP_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[test]
    fn request_shape_matches_magicpaper_runtime_contract() {
        let id = AppId::new("koreader").unwrap();
        let request = RuntimeRequest {
            version: 1,
            request_id: "test-1".into(),
            source: "daemon",
            command: "open_app",
            app: Some(id.as_str()),
            open_path: Some(Path::new("/home/root/books/論語.epub")),
        };
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["source"], "daemon");
        assert_eq!(value["command"], "open_app");
        assert_eq!(value["app"], "koreader");
        assert_eq!(value["open_path"], "/home/root/books/論語.epub");
    }

    #[tokio::test]
    async fn newline_runtime_round_trip_accepts_an_ack() {
        let (client, server_stream) = tokio::io::duplex(2048);
        let server = tokio::spawn(async move {
            let mut line = String::new();
            let mut stream = BufReader::new(server_stream);
            stream.read_line(&mut line).await.unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["source"], "daemon");
            assert_eq!(request["command"], "open_app");
            stream
                .get_mut()
                .write_all(b"{\"ok\":true,\"status\":\"accepted\"}\n")
                .await
                .unwrap();
        });
        let body = b"{\"source\":\"daemon\",\"command\":\"open_app\"}\n";
        let reply = exchange(client, body, "open_app").await.unwrap();
        assert!(reply.ok);
        server.await.unwrap();
    }

    #[test]
    fn parses_first_frame_readiness_snapshot() {
        let reply: RuntimeReply = serde_json::from_str(
            r#"{"ok":true,"status":"ok","active_app":"magicpaper","state":"foreground","qtfb_connected":true,"first_frame":true,"full_refresh_complete":true,"foreground_epoch":11,"generation":7}"#,
        )
        .unwrap();
        assert_eq!(reply.active_app, "magicpaper");
        assert_eq!(reply.state, "foreground");
        assert!(reply.qtfb_connected);
        assert!(reply.first_frame);
        assert!(reply.full_refresh_complete);
        assert_eq!(reply.generation, 7);
        assert_eq!(reply.foreground_epoch, 11);
    }

    #[test]
    fn parses_per_app_lifecycle_snapshot() {
        let reply: RuntimeReply = serde_json::from_str(
            r#"{"ok":true,"status":"ok","app_statuses":{"koreader":{"state":"background","generation":3,"foreground_epoch":8,"qtfb_connected":true,"first_frame":true,"full_refresh_complete":true}}}"#,
        )
        .unwrap();
        let app = reply.app_statuses.get("koreader").unwrap();
        assert_eq!(app.state, "background");
        assert_eq!(app.generation, 3);
        assert_eq!(app.foreground_epoch, 8);
        assert!(app.qtfb_connected);
        assert!(app.first_frame);
        assert!(app.full_refresh_complete);
        assert!(app.error.is_empty());
    }

    #[test]
    fn app_stop_timeout_covers_koreader_fallback_kill() {
        assert!(APP_STOP_TIMEOUT > Duration::from_millis(5500));
    }

    #[test]
    fn reports_per_app_error_after_runtime_returns_to_manager() {
        let id = AppId::new("koreader").unwrap();
        let reply: RuntimeReply = serde_json::from_str(
            r#"{"ok":true,"status":"ok","active_app":"","app_statuses":{"koreader":{"state":"error","error":"semantic_ready_timeout","generation":9}}}"#,
        )
        .unwrap();
        let status = reply.app_statuses.get(id.as_str()).unwrap();
        assert_eq!(
            terminal_launch_error(&id, status).as_deref(),
            Some("runtime could not launch koreader: semantic_ready_timeout")
        );
    }
}
