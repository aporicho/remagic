use super::{last_submission_sequence, protocol::request, status, Command, Snapshot, WireRect};
use remagic_protocol::{LOCK_UNLOCK_HEIGHT, LOCK_UNLOCK_WIDTH, LOCK_UNLOCK_X, LOCK_UNLOCK_Y};
use std::time::Duration;

pub async fn show_lock(
    key: i32,
    generation: u64,
    foreground_epoch: u64,
    sleep_epoch: u64,
) -> Result<(), String> {
    let before = status().await?;
    let baseline = last_submission_sequence(&before);
    request(&Command::ShowLock {
        key,
        generation,
        foreground_epoch,
        sleep_epoch,
        unlock_region: WireRect {
            x: LOCK_UNLOCK_X,
            y: LOCK_UNLOCK_Y,
            width: LOCK_UNLOCK_WIDTH,
            height: LOCK_UNLOCK_HEIGHT,
        },
    })
    .await?;
    // A retry after losing the first acknowledgement accepts the exact lock
    // lease already committed by the host instead of demanding duplicate I/O.
    let current = status().await?;
    if lock_is_committed_as(&current, key, generation, foreground_epoch, sleep_epoch) {
        return Ok(());
    }
    wait_for_lock_submission(
        baseline,
        before.panel_failure_count,
        key,
        generation,
        foreground_epoch,
        sleep_epoch,
        "lock_screen",
    )
    .await
}

pub(super) fn lock_is_committed_as(
    snapshot: &Snapshot,
    key: i32,
    generation: u64,
    foreground_epoch: u64,
    sleep_epoch: u64,
) -> bool {
    snapshot.lock_epoch == sleep_epoch
        && snapshot.lock_committed
        && snapshot.matches_foreground(key, generation, foreground_epoch)
}

pub async fn cancel_lock(
    sleep_epoch: u64,
    replacement_surface_sequence: u64,
) -> Result<(), String> {
    let before = status().await?;
    if before.lock_epoch != sleep_epoch || !before.lock_committed {
        return Err(format!(
            "display host has no committed lock epoch {sleep_epoch} to replace"
        ));
    }
    let baseline = last_submission_sequence(&before);
    let baseline_failures = before.panel_failure_count;
    let key = before
        .foreground_key
        .ok_or_else(|| "display lock has no foreground surface".to_string())?;
    let generation = before.generation;
    let foreground_epoch = before.foreground_epoch;
    request(&Command::CancelLock {
        sleep_epoch,
        replacement_surface_sequence,
    })
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = status().await?;
        if snapshot.panel_failure_count > baseline_failures {
            return Err(format!(
                "display host reported a new panel failure while unlocking epoch {sleep_epoch}"
            ));
        }
        let replacement_submitted = snapshot.recent_submissions.iter().any(|submission| {
            submission.sequence > baseline
                && submission.key == key
                && submission.generation == generation
                && submission.foreground_epoch == foreground_epoch
                && submission.intent == "full"
                && submission.reason == "unlock_screen"
                && submission.surface_sequence >= replacement_surface_sequence
                && submission.visible_signature != 0
                && submission.success
                && submission.marker.is_some()
        });
        if snapshot.lock_epoch == 0 && replacement_submitted {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "display host retained lock epoch {sleep_epoch} after cancellation"
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn wait_for_lock_submission(
    baseline: u64,
    baseline_failures: u64,
    key: i32,
    generation: u64,
    foreground_epoch: u64,
    sleep_epoch: u64,
    reason: &str,
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = status().await?;
        if snapshot.panel_failure_count > baseline_failures {
            return Err(format!(
                "display host reported a new panel failure while committing lock epoch {sleep_epoch}"
            ));
        }
        let submitted = snapshot.recent_submissions.iter().any(|submission| {
            submission.sequence > baseline
                && submission.key == key
                && submission.generation == generation
                && submission.foreground_epoch == foreground_epoch
                && submission.intent == "full"
                && submission.reason == reason
                && submission.surface_sequence > 0
                && submission.visible_signature != 0
                && submission.success
                && submission.marker.is_some()
        });
        if snapshot.lock_epoch == sleep_epoch && snapshot.lock_committed && submitted {
            return Ok(());
        }
        if snapshot.panel_failure_count > 0 {
            return Err(format!(
                "display host reported {} panel submission failure(s) while presenting lock",
                snapshot.panel_failure_count
            ));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "display host did not commit lock epoch {sleep_epoch} ({reason})"
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
