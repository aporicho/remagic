use remagic_core::AppId;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::Duration;

mod lock;
mod protocol;

#[cfg(test)]
use lock::lock_is_committed_as;
pub use lock::{cancel_lock, show_lock};
use protocol::{request, Command};

pub const SOCKET: &str = "/run/remagic/display.sock";
pub const HOME_SURFACE_KEY: i32 = remagic_core::REMAGIC_HOME_QTFB_KEY;

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
    pub lock_epoch: u64,
    #[serde(default)]
    pub lock_committed: bool,
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

pub async fn wait_surface_sequence(
    key: i32,
    expected_sequence: u64,
    timeout: Duration,
) -> Result<Snapshot, String> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let snapshot = status().await?;
        let observed = snapshot.surface_sequences.get(&key).copied().unwrap_or(0);
        if snapshot.surfaces.contains(&key)
            && observed >= expected_sequence
            && snapshot.surface_signatures.get(&key).copied().unwrap_or(0) != 0
        {
            return Ok(snapshot);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "surface {key} did not commit lock sequence {expected_sequence}; observed {observed}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
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
    let baseline_failures = baseline.panel_failure_count;
    request(&Command::SetForeground {
        key,
        generation,
        foreground_epoch,
        full_refresh,
    })
    .await?;
    wait_for_foreground_submission(
        baseline_sequence,
        baseline_failures,
        key,
        generation,
        foreground_epoch,
        full_refresh,
    )
    .await?;
    Ok(())
}

pub async fn prepare_foreground(
    key: i32,
    generation: u64,
    foreground_epoch: u64,
) -> Result<(), String> {
    request(&Command::PrepareForeground {
        key,
        generation,
        foreground_epoch,
    })
    .await
    .map(|_| ())
}

pub async fn activate_foreground(
    key: i32,
    generation: u64,
    foreground_epoch: u64,
    ink_enabled: bool,
    full_refresh: bool,
) -> Result<(), String> {
    let baseline = status().await?;
    let baseline_sequence = last_submission_sequence(&baseline);
    request(&Command::ActivateForeground {
        key,
        generation,
        foreground_epoch,
        ink_enabled,
        full_refresh,
    })
    .await?;
    let snapshot = wait_for_foreground_submission(
        baseline_sequence,
        baseline.panel_failure_count,
        key,
        generation,
        foreground_epoch,
        full_refresh,
    )
    .await?;
    if snapshot.ink_enabled != ink_enabled {
        return Err(format!(
            "display host committed foreground without its ink policy: expected={ink_enabled} actual={}",
            snapshot.ink_enabled
        ));
    }
    Ok(())
}

async fn wait_for_foreground_submission(
    baseline_sequence: u64,
    baseline_failures: u64,
    key: i32,
    generation: u64,
    foreground_epoch: u64,
    full_refresh: bool,
) -> Result<Snapshot, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = status().await?;
        if snapshot.panel_failure_count > baseline_failures {
            return Err(format!(
                "display host reported a new panel submission failure ({} -> {})",
                baseline_failures, snapshot.panel_failure_count
            ));
        }
        if snapshot.has_presented(key, generation, foreground_epoch) {
            let submitted = snapshot.recent_submissions.iter().any(|submission| {
                submission.sequence > baseline_sequence
                    && submission.key == key
                    && submission.generation == generation
                    && submission.foreground_epoch == foreground_epoch
                    && (if full_refresh {
                        submission.intent == "full"
                    } else {
                        matches!(submission.intent.as_str(), "mono_quality" | "content")
                    })
                    && submission.reason == "foreground_switch"
                    && submission.surface_sequence > 0
                    && submission.visible_signature != 0
                    && submission.success
                    && submission.marker.is_some()
            });
            if submitted {
                return Ok(snapshot);
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

fn last_submission_sequence(snapshot: &Snapshot) -> u64 {
    snapshot
        .recent_submissions
        .last()
        .map_or(0, |submission| submission.sequence)
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

    #[test]
    fn idempotent_lock_retry_requires_the_exact_committed_fence() {
        let snapshot = Snapshot {
            foreground_key: Some(9),
            generation: 3,
            foreground_epoch: 11,
            lock_epoch: 17,
            lock_committed: true,
            ..Snapshot::default()
        };
        assert!(lock_is_committed_as(&snapshot, 9, 3, 11, 17));
        assert!(!lock_is_committed_as(&snapshot, 9, 3, 10, 17));
        assert!(!lock_is_committed_as(&snapshot, 9, 3, 11, 16));
        let uncommitted = Snapshot {
            lock_committed: false,
            ..snapshot
        };
        assert!(!lock_is_committed_as(&uncommitted, 9, 3, 11, 17));
    }
}
