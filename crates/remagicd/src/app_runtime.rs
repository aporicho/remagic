use remagic_core::AppId;
use remagic_protocol::{read_frame, write_frame, AppCommand, Response};
use serde::Deserialize;
use std::fs;
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UnixStream;

const CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_STATUS_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct LifecycleStatus {
    pub schema: u32,
    pub app_id: AppId,
    pub generation: u64,
    pub foreground_epoch: u64,
    #[serde(default)]
    pub lease_id: Option<u64>,
    pub request_id: String,
    pub event: String,
    #[serde(default)]
    pub first_frame_sequence: Option<u64>,
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub state_saved: bool,
    #[serde(default)]
    pub background_ready: bool,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub resume_payload: Option<serde_json::Value>,
}

pub async fn command(runtime_dir: &Path, command: &AppCommand) -> Result<(), String> {
    let path = runtime_dir.join("control.sock");
    let operation = async {
        let mut stream = UnixStream::connect(&path)
            .await
            .map_err(|error| format!("cannot connect to {}: {error}", path.display()))?;
        write_frame(&mut stream, command)
            .await
            .map_err(|error| format!("cannot write lifecycle command: {error}"))?;
        match read_frame::<_, Response>(&mut stream)
            .await
            .map_err(|error| format!("cannot read lifecycle acknowledgement: {error}"))?
        {
            Response::Ok => Ok(()),
            Response::Error { message } => {
                Err(format!("application rejected lifecycle command: {message}"))
            }
            response => Err(format!(
                "unexpected lifecycle acknowledgement: {response:?}"
            )),
        }
    };
    tokio::time::timeout(CONTROL_TIMEOUT, operation)
        .await
        .map_err(|_| {
            format!(
                "application lifecycle command timed out at {}",
                path.display()
            )
        })?
}

pub async fn wait_event(
    runtime_dir: &Path,
    app_id: &AppId,
    generation: u64,
    foreground_epoch: u64,
    lease_id: u64,
    expected_event: &str,
    timeout: Duration,
) -> Result<LifecycleStatus, String> {
    wait_status(
        runtime_dir,
        app_id,
        generation,
        foreground_epoch,
        lease_id,
        expected_event,
        false,
        timeout,
    )
    .await
}

pub async fn wait_background_ready(
    runtime_dir: &Path,
    app_id: &AppId,
    generation: u64,
    foreground_epoch: u64,
    lease_id: u64,
    timeout: Duration,
) -> Result<LifecycleStatus, String> {
    wait_status(
        runtime_dir,
        app_id,
        generation,
        foreground_epoch,
        lease_id,
        "state_saved + background_ready",
        true,
        timeout,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn wait_status(
    runtime_dir: &Path,
    app_id: &AppId,
    generation: u64,
    foreground_epoch: u64,
    lease_id: u64,
    expected_event: &str,
    require_park_milestones: bool,
    timeout: Duration,
) -> Result<LifecycleStatus, String> {
    let path = runtime_dir.join("lifecycle-status.json");
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let last_observation = match read_status(&path) {
            Ok(status)
                if status.app_id == *app_id
                    && status.generation == generation
                    && status.foreground_epoch == foreground_epoch
                    && status.lease_id == Some(lease_id) =>
            {
                if status.event == "failed" {
                    return Err(format!(
                        "application failed during {}: {}",
                        status.stage.as_deref().unwrap_or("runtime"),
                        status.message.as_deref().unwrap_or("unspecified failure")
                    ));
                }
                if (require_park_milestones && status.state_saved && status.background_ready)
                    || (!require_park_milestones && status.event == expected_event)
                {
                    return Ok(status);
                }
                format!("current event is {} ({})", status.event, status.request_id)
            }
            Ok(status) => format!(
                "stale token {}/{}/{}/{:?}",
                status.app_id, status.generation, status.foreground_epoch, status.lease_id
            ),
            Err(error) => error,
        };
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "application {} did not publish {expected_event} for generation {generation} epoch {foreground_epoch} lease {lease_id} within {} ms: {last_observation}",
                app_id,
                timeout.as_millis()
            ));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[cfg(test)]
fn status_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("lifecycle-status.json")
}

fn read_status(path: &Path) -> Result<LifecycleStatus, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_STATUS_BYTES {
        return Err(format!(
            "invalid lifecycle status file at {}",
            path.display()
        ));
    }
    let bytes =
        fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let status: LifecycleStatus = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid lifecycle status at {}: {error}", path.display()))?;
    if status.schema != 1
        || status.generation == 0
        || status.foreground_epoch == 0
        || status.request_id.is_empty()
        || status.event.is_empty()
        || status.lease_id == Some(0)
        || status.first_frame_sequence == Some(0)
    {
        return Err(format!(
            "invalid lifecycle status fields at {}",
            path.display()
        ));
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn temporary_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "remagicd-lifecycle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[tokio::test]
    async fn wait_event_ignores_wrong_lease_then_accepts_ready() {
        let root = temporary_dir();
        let app = AppId::new("magicpaper").unwrap();
        fs::write(
            status_path(&root),
            br#"{"schema":1,"app_id":"magicpaper","generation":4,"foreground_epoch":7,"lease_id":8,"request_id":"old","event":"ready","first_frame_sequence":1}"#,
        )
        .unwrap();
        let writer_root = root.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            fs::write(
                status_path(&writer_root),
                br#"{"schema":1,"app_id":"magicpaper","generation":4,"foreground_epoch":7,"lease_id":9,"request_id":"new","event":"ready","first_frame_sequence":5}"#,
            )
            .unwrap();
        });
        let status = wait_event(&root, &app, 4, 7, 9, "ready", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(status.first_frame_sequence, Some(5));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_event_is_returned_without_waiting_for_timeout() {
        let root = temporary_dir();
        let app = AppId::new("koreader").unwrap();
        fs::write(
            status_path(&root),
            br#"{"schema":1,"app_id":"koreader","generation":2,"foreground_epoch":3,"lease_id":3,"request_id":"failed","event":"failed","stage":"start","message":"database"}"#,
        )
        .unwrap();
        let error = wait_event(&root, &app, 2, 3, 3, "ready", Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(error.contains("database"));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn park_waits_for_both_milestones_and_returns_real_payload() {
        let root = temporary_dir();
        let app = AppId::new("koreader").unwrap();
        fs::write(
            status_path(&root),
            br#"{"schema":1,"app_id":"koreader","generation":2,"foreground_epoch":8,"lease_id":9,"request_id":"saved","event":"state_saved","state_saved":true,"background_ready":false,"resume_payload":{"page":41}}"#,
        )
        .unwrap();
        let writer_root = root.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            fs::write(
                status_path(&writer_root),
                r#"{"schema":1,"app_id":"koreader","generation":2,"foreground_epoch":8,"lease_id":9,"request_id":"parked","event":"background_ready","state_saved":true,"background_ready":true,"title":"KOReader","subtitle":"第 41 页","resume_payload":{"page":41}}"#,
            )
            .unwrap();
        });
        let status = wait_background_ready(&root, &app, 2, 8, 9, Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(status.title.as_deref(), Some("KOReader"));
        assert_eq!(status.resume_payload, Some(serde_json::json!({"page": 41})));
        fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn timed_out_park_ack_cannot_satisfy_a_new_foreground_fence() {
        let root = temporary_dir();
        let app = AppId::new("magicpaper").unwrap();
        let error = wait_background_ready(&root, &app, 6, 11, 11, Duration::from_millis(25))
            .await
            .unwrap_err();
        assert!(error.contains("did not publish state_saved + background_ready"));

        // The old park finally completes after its caller has already begun
        // recovery. Its exact old token must not count as readiness for the
        // replacement foreground lease.
        fs::write(
            status_path(&root),
            br#"{"schema":1,"app_id":"magicpaper","generation":6,"foreground_epoch":11,"lease_id":11,"request_id":"late-park","event":"background_ready","state_saved":true,"background_ready":true}"#,
        )
        .unwrap();
        let writer_root = root.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            fs::write(
                status_path(&writer_root),
                br#"{"schema":1,"app_id":"magicpaper","generation":6,"foreground_epoch":12,"lease_id":12,"request_id":"restored","event":"ready","first_frame_sequence":9}"#,
            )
            .unwrap();
        });
        let status = wait_event(&root, &app, 6, 12, 12, "ready", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(status.request_id, "restored");
        assert_eq!(status.foreground_epoch, 12);
        fs::remove_dir_all(root).unwrap();
    }
}
