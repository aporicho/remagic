use remagic_core::AppId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub const SOCKET: &str = "/run/remagic/display.sock";
pub const HOME_SURFACE_KEY: i32 = remagic_core::REMAGIC_HOME_QTFB_KEY;
const MAX_REPLY_BYTES: u64 = 64 * 1024;
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// Stable compatibility surface key. The high bit range is reserved for
/// managed applications, keeping it disjoint from the manager-home surface.
pub fn app_surface_key(app: &AppId) -> i32 {
    remagic_core::qtfb_key_for_app(app)
}

#[derive(Debug, Default, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub surfaces: Vec<i32>,
    #[serde(default)]
    pub surface_sequences: BTreeMap<i32, u64>,
    #[serde(default)]
    pub surface_signatures: BTreeMap<i32, u64>,
    #[serde(default)]
    pub foreground_key: Option<i32>,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub foreground_epoch: u64,
    #[serde(default)]
    pub ink_enabled: bool,
    #[serde(default)]
    pub panel_failure_count: u64,
    #[serde(default)]
    pub last_presented_key: Option<i32>,
    #[serde(default)]
    pub last_presented_sequence: u64,
    #[serde(default)]
    pub recent_submissions: Vec<SubmissionRecord>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SubmissionRecord {
    pub sequence: u64,
    pub surface_sequence: u64,
    pub key: i32,
    pub generation: u64,
    pub foreground_epoch: u64,
    pub intent: String,
    pub reason: String,
    pub visible_signature: u64,
    #[serde(default)]
    pub marker: Option<u64>,
    pub success: bool,
}

#[derive(Debug, Deserialize)]
struct Reply {
    protocol: u32,
    request_id: String,
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    snapshot: Option<Snapshot>,
}

#[derive(Serialize)]
struct Envelope<'a, T: Serialize> {
    protocol: u32,
    request_id: String,
    #[serde(flatten)]
    command: &'a T,
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Command {
    Status,
    SetForeground {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
        full_refresh: bool,
    },
    ClearForeground,
    ConfigureInk {
        key: i32,
        generation: u64,
        foreground_epoch: u64,
        enabled: bool,
    },
    RequestFullRefresh,
}

pub async fn wait_ready() -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    loop {
        if status().await.is_ok() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "display host did not publish a healthy control socket at {SOCKET}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

pub async fn wait_surface(key: i32, timeout: Duration) -> Result<Snapshot, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last_error = None;
    loop {
        match status().await {
            Ok(snapshot)
                if snapshot.surfaces.contains(&key)
                    && snapshot.surface_sequences.get(&key).copied().unwrap_or(0) > 0 =>
            {
                return Ok(snapshot)
            }
            Ok(_) => {}
            Err(error) => last_error = Some(error),
        }
        if tokio::time::Instant::now() >= deadline {
            let mut message = format!(
                "application surface {key} did not connect within {} ms",
                timeout.as_millis()
            );
            if let Some(error) = last_error {
                message.push_str(&format!("; last display-host error: {error}"));
            }
            return Err(message);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

pub async fn status() -> Result<Snapshot, String> {
    request(&Command::Status)
        .await?
        .snapshot
        .ok_or_else(|| "display host omitted its status snapshot".into())
}

pub async fn set_foreground(
    key: i32,
    generation: u64,
    foreground_epoch: u64,
    full_refresh: bool,
) -> Result<(), String> {
    let baseline = status().await?;
    let baseline_sequence = baseline
        .recent_submissions
        .last()
        .map_or(0, |submission| submission.sequence);
    request(&Command::SetForeground {
        key,
        generation,
        foreground_epoch,
        full_refresh,
    })
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = status().await?;
        if snapshot.panel_failure_count > 0 {
            return Err(format!(
                "display host reported {} panel submission failure(s)",
                snapshot.panel_failure_count
            ));
        }
        if snapshot.has_presented(key, generation, foreground_epoch) {
            let expected_intent = if full_refresh { "full" } else { "content" };
            let submitted = snapshot.recent_submissions.iter().any(|submission| {
                submission.sequence > baseline_sequence
                    && submission.key == key
                    && submission.generation == generation
                    && submission.foreground_epoch == foreground_epoch
                    && submission.intent == expected_intent
                    && submission.reason == "foreground_switch"
                    && submission.surface_sequence > 0
                    && submission.visible_signature != 0
                    && submission.success
                    && submission.marker.is_some()
            });
            if submitted {
                return Ok(());
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "display host did not present foreground key={key} generation={generation} epoch={foreground_epoch}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub async fn clear_foreground() -> Result<(), String> {
    request(&Command::ClearForeground).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = status().await?;
        if snapshot.foreground_key.is_none() {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "display host retained committed foreground key {:?} after clear",
                snapshot.foreground_key
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub async fn configure_ink(
    key: i32,
    generation: u64,
    foreground_epoch: u64,
    enabled: bool,
) -> Result<(), String> {
    request(&Command::ConfigureInk {
        key,
        generation,
        foreground_epoch,
        enabled,
    })
    .await?;
    let snapshot = status().await?;
    if snapshot.matches_foreground(key, generation, foreground_epoch)
        && snapshot.ink_enabled == enabled
    {
        Ok(())
    } else {
        Err(format!(
            "display host did not retain ink fence key={key} generation={generation} epoch={foreground_epoch} enabled={enabled}"
        ))
    }
}

pub async fn full_refresh() -> Result<(), String> {
    request(&Command::RequestFullRefresh).await.map(|_| ())
}

async fn request(command: &Command) -> Result<Reply, String> {
    let request_id = format!(
        "remagicd-{}-{}",
        std::process::id(),
        NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
    );
    let envelope = Envelope {
        protocol: 1,
        request_id: request_id.clone(),
        command,
    };
    let mut bytes = serde_json::to_vec(&envelope).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    let operation = async {
        let mut stream = UnixStream::connect(SOCKET)
            .await
            .map_err(|error| format!("cannot connect to display host: {error}"))?;
        stream
            .write_all(&bytes)
            .await
            .map_err(|error| format!("cannot write display command: {error}"))?;
        stream
            .shutdown()
            .await
            .map_err(|error| format!("cannot finish display command: {error}"))?;
        let mut line = String::new();
        let mut reader = BufReader::new(stream).take(MAX_REPLY_BYTES);
        let count = reader
            .read_line(&mut line)
            .await
            .map_err(|error| format!("cannot read display reply: {error}"))?;
        if count == 0 || !line.ends_with('\n') {
            return Err("display host closed without a complete reply".into());
        }
        let reply: Reply = serde_json::from_str(&line)
            .map_err(|error| format!("invalid display reply: {error}"))?;
        if reply.protocol != 1 || reply.request_id != request_id {
            return Err(format!(
                "display host reply identity mismatch: protocol={} request_id={}",
                reply.protocol, reply.request_id
            ));
        }
        if reply.ok {
            Ok(reply)
        } else {
            Err(reply
                .error
                .unwrap_or_else(|| "display host rejected the command".into()))
        }
    };
    tokio::time::timeout(Duration::from_secs(3), operation)
        .await
        .map_err(|_| "display host command timed out".to_string())?
}

impl Snapshot {
    fn matches_foreground(&self, key: i32, generation: u64, foreground_epoch: u64) -> bool {
        self.foreground_key == Some(key)
            && self.generation == generation
            && self.foreground_epoch == foreground_epoch
    }

    fn has_presented(&self, key: i32, generation: u64, foreground_epoch: u64) -> bool {
        self.matches_foreground(key, generation, foreground_epoch)
            && self.last_presented_key == Some(key)
            && self.last_presented_sequence > 0
            && self.surface_signatures.get(&key).copied().unwrap_or(0) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_keys_are_stable_positive_and_not_home() {
        let magicpaper = AppId::new("magicpaper").unwrap();
        let koreader = AppId::new("koreader").unwrap();
        assert_eq!(app_surface_key(&magicpaper), app_surface_key(&magicpaper));
        assert_ne!(app_surface_key(&magicpaper), app_surface_key(&koreader));
        assert_ne!(app_surface_key(&magicpaper), HOME_SURFACE_KEY);
        assert!(app_surface_key(&magicpaper) > 0);
    }

    #[test]
    fn foreground_snapshot_requires_the_complete_fence() {
        let snapshot = Snapshot {
            foreground_key: Some(9),
            generation: 3,
            foreground_epoch: 11,
            ..Snapshot::default()
        };
        assert!(snapshot.matches_foreground(9, 3, 11));
        assert!(!snapshot.matches_foreground(9, 2, 11));
        assert!(!snapshot.matches_foreground(9, 3, 10));
        assert!(!snapshot.matches_foreground(8, 3, 11));
    }

    #[test]
    fn presentation_requires_matching_nonempty_surface_telemetry() {
        let mut snapshot = Snapshot {
            foreground_key: Some(9),
            generation: 3,
            foreground_epoch: 11,
            last_presented_key: Some(9),
            last_presented_sequence: 4,
            ..Snapshot::default()
        };
        assert!(!snapshot.has_presented(9, 3, 11));
        snapshot.surface_signatures.insert(9, 1234);
        assert!(snapshot.has_presented(9, 3, 11));
        snapshot.last_presented_key = Some(8);
        assert!(!snapshot.has_presented(9, 3, 11));
    }
}
