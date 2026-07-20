use remagic_core::{AppId, ManifestStore};
use remagic_protocol::{read_frame, write_frame, Request, Response};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::net::UnixStream;
use tokio::process::Command;

const MANIFEST_ROOT: &str = "/home/root/.local/share/remagic/apps.d";
const DISPLAY_LOCK: &str = "/run/remagic/display.lock";
const LAUNCH_ROOT: &str = "/run/remagic/launch";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let raw_id = std::env::args()
        .nth(1)
        .ok_or("usage: remagic-runner <app-id>")?;
    let id = AppId::new(raw_id)?;
    let root = std::env::var_os("REMAGIC_MANIFEST_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| MANIFEST_ROOT.into());
    let manifest = ManifestStore::new(root)
        .load_all()?
        .remove(&id)
        .ok_or("application is not registered")?;

    fs::create_dir_all("/run/remagic")?;
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(DISPLAY_LOCK)?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }

    let launch_path = Path::new(LAUNCH_ROOT).join(format!("{}.json", id.as_str()));
    let launch: Value = fs::read(&launch_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let _ = fs::remove_file(&launch_path);
    let open_path = launch
        .get("open_path")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let resume_payload = launch
        .get("resume_payload")
        .cloned()
        .filter(|v| !v.is_null());

    let mut command = Command::new(&manifest.exec);
    command
        .args(&manifest.args)
        .current_dir(&manifest.working_dir);
    command.envs(&manifest.environment);
    command.env("REMAGIC_SOCKET", remagic_protocol::DEFAULT_SOCKET);
    command.env("REMAGIC_APP_ID", id.as_str());
    command.env(
        "REMAGIC_LAUNCH_ID",
        format!("{}-{}", id, std::process::id()),
    );
    command.env("REMAGIC_MANAGED", "1");
    if let Some(payload) = &resume_payload {
        command.env("REMAGIC_RESUME_PAYLOAD", serde_json::to_string(payload)?);
    }
    if let Some(path) = &open_path {
        command.arg(path);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut child = command.spawn()?;
    let _ = send(Request::Ready { app_id: id.clone() }).await;
    let status = child.wait().await?;
    let title = manifest.name.clone();
    let subtitle = if status.success() {
        "已暂停，可继续".to_string()
    } else {
        format!("异常退出：{status}")
    };
    let payload = if let Some(path) = open_path {
        Some(serde_json::json!({ "open_path": path }))
    } else {
        resume_payload
    };
    let _ = send(Request::Parked {
        app_id: id,
        title,
        subtitle,
        resume_payload: payload,
    })
    .await;
    drop(lock);
    if status.success() {
        Ok(())
    } else {
        std::process::exit(status.code().unwrap_or(1));
    }
}

async fn send(request: Request) -> Result<Response, Box<dyn std::error::Error>> {
    let mut stream = UnixStream::connect(remagic_protocol::DEFAULT_SOCKET).await?;
    write_frame(&mut stream, &request).await?;
    Ok(read_frame(&mut stream).await?)
}
