use std::sync::Mutex;

const RETAINED_LOCK_SUFFIX: &str = "; lock screen retained";

pub(super) fn retained_lock_error(cause: &str) -> String {
    format!("{cause}{RETAINED_LOCK_SUFFIX}")
}

pub(super) fn is_retained_lock_error(error: &str) -> bool {
    error.ends_with(RETAINED_LOCK_SUFFIX)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum SleepPhase {
    #[default]
    Idle,
    Preparing,
    Locked,
    Unlocking,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SleepSnapshot {
    pub(super) epoch: u64,
    pub(super) phase: SleepPhase,
    /// Changes on every sleep transaction phase transition. Supervision
    /// events carry this value so observations made before a wake cannot tear
    /// down a newly unlocked manager session.
    pub(super) revision: u64,
}

#[derive(Debug, Default)]
pub(super) struct SleepTransaction(Mutex<SleepSnapshot>);

impl SleepTransaction {
    pub(super) fn snapshot(&self) -> SleepSnapshot {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(super) fn begin(&self, epoch: u64) -> Result<SleepSnapshot, String> {
        if epoch == 0 {
            return Err("sleep transaction epoch must be nonzero".into());
        }
        self.update(|current| {
            if current.phase != SleepPhase::Idle {
                return Err(format!(
                    "sleep transaction is already {:?} at epoch {}",
                    current.phase, current.epoch
                ));
            }
            Ok((epoch, SleepPhase::Preparing))
        })
    }

    pub(super) fn mark_locked(&self, epoch: u64) -> Result<SleepSnapshot, String> {
        self.update(|current| {
            if current.epoch != epoch || current.phase != SleepPhase::Preparing {
                return Err(format!(
                    "cannot commit sleep epoch {epoch} from {:?} epoch {}",
                    current.phase, current.epoch
                ));
            }
            Ok((epoch, SleepPhase::Locked))
        })
    }

    pub(super) fn begin_unlock(&self, epoch: u64) -> Result<SleepSnapshot, String> {
        self.update(|current| {
            if current.epoch != epoch
                || !matches!(current.phase, SleepPhase::Preparing | SleepPhase::Locked)
            {
                return Err(format!(
                    "cannot unlock sleep epoch {epoch} from {:?} epoch {}",
                    current.phase, current.epoch
                ));
            }
            Ok((epoch, SleepPhase::Unlocking))
        })
    }

    /// Start a fresh awake-lock interval after physical resume.
    /// The revision invalidates an older auto-resuspend timer while retaining
    /// the exact display lock epoch and retryable Locked phase.
    pub(super) fn mark_awake(&self, epoch: u64) -> Result<SleepSnapshot, String> {
        self.update(|current| {
            if current.epoch != epoch || current.phase != SleepPhase::Locked {
                return Err(format!(
                    "cannot mark sleep epoch {epoch} awake from {:?} epoch {}",
                    current.phase, current.epoch
                ));
            }
            Ok((epoch, SleepPhase::Locked))
        })
    }

    pub(super) fn finish_unlock(&self, epoch: u64) -> Result<SleepSnapshot, String> {
        self.update(|current| {
            if current.epoch != epoch || current.phase != SleepPhase::Unlocking {
                return Err(format!(
                    "cannot finish sleep epoch {epoch} from {:?} epoch {}",
                    current.phase, current.epoch
                ));
            }
            Ok((0, SleepPhase::Idle))
        })
    }

    /// A wake attempt may fail before the display host atomically replaces the
    /// frozen lock frame. Restore the committed Locked phase so a later touch
    /// can retry the same sleep epoch instead of stranding the transaction in
    /// Unlocking forever.
    pub(super) fn abort_unlock(&self, epoch: u64) -> Result<SleepSnapshot, String> {
        self.update(|current| {
            if current.epoch != epoch || current.phase != SleepPhase::Unlocking {
                return Err(format!(
                    "cannot abort unlock epoch {epoch} from {:?} epoch {}",
                    current.phase, current.epoch
                ));
            }
            Ok((epoch, SleepPhase::Locked))
        })
    }

    /// Force an idle transaction while leaving a new revision fence behind.
    /// System restoration owns the complete display domain and may therefore
    /// retire even a partially failed transaction.
    pub(super) fn reset(&self) -> Result<SleepSnapshot, String> {
        self.update(|_| Ok((0, SleepPhase::Idle)))
    }

    fn update(
        &self,
        transition: impl FnOnce(SleepSnapshot) -> Result<(u64, SleepPhase), String>,
    ) -> Result<SleepSnapshot, String> {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (epoch, phase) = transition(*state)?;
        let revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| "sleep transaction revision overflow".to_string())?;
        *state = SleepSnapshot {
            epoch,
            phase,
            revision,
        };
        Ok(*state)
    }
}

pub(super) fn recovery_fence_matches(
    current_state_sequence: u64,
    current_sleep_revision: u64,
    observed_state_sequence: u64,
    observed_sleep_revision: u64,
) -> bool {
    current_state_sequence == observed_state_sequence
        && current_sleep_revision == observed_sleep_revision
}

pub(super) fn resuspend_fence_matches(
    current: SleepSnapshot,
    expected_epoch: u64,
    expected_revision: u64,
    current_interaction_epoch: u64,
    expected_interaction_epoch: u64,
) -> bool {
    current.phase == SleepPhase::Locked
        && current.epoch == expected_epoch
        && current.revision == expected_revision
        && current_interaction_epoch == expected_interaction_epoch
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_lock_marker_is_exact_and_terminal() {
        let marked = retained_lock_error("charger blocked suspend");
        assert!(is_retained_lock_error(&marked));
        assert!(!is_retained_lock_error(
            "lock screen retained but recovery failed"
        ));
    }

    #[test]
    fn sleep_transaction_is_fenced_across_every_phase() {
        let transaction = SleepTransaction::default();
        let idle = transaction.snapshot();
        let preparing = transaction.begin(7).unwrap();
        let locked = transaction.mark_locked(7).unwrap();
        let unlocking = transaction.begin_unlock(7).unwrap();
        let finished = transaction.finish_unlock(7).unwrap();

        assert_eq!(idle.phase, SleepPhase::Idle);
        assert_eq!(preparing.phase, SleepPhase::Preparing);
        assert_eq!(locked.phase, SleepPhase::Locked);
        assert_eq!(unlocking.phase, SleepPhase::Unlocking);
        assert_eq!(finished.phase, SleepPhase::Idle);
        assert_eq!(finished.epoch, 0);
        assert!(idle.revision < preparing.revision);
        assert!(preparing.revision < locked.revision);
        assert!(locked.revision < unlocking.revision);
        assert!(unlocking.revision < finished.revision);
    }

    #[test]
    fn stale_epochs_cannot_advance_or_finish_a_transaction() {
        let transaction = SleepTransaction::default();
        transaction.begin(9).unwrap();
        assert!(transaction.mark_locked(8).is_err());
        transaction.mark_locked(9).unwrap();
        assert!(transaction.begin_unlock(8).is_err());
        transaction.begin_unlock(9).unwrap();
        assert!(transaction.finish_unlock(8).is_err());
        transaction.finish_unlock(9).unwrap();
    }

    #[test]
    fn failed_unlock_returns_to_the_retryable_locked_phase() {
        let transaction = SleepTransaction::default();
        transaction.begin(13).unwrap();
        transaction.mark_locked(13).unwrap();
        transaction.begin_unlock(13).unwrap();

        let locked = transaction.abort_unlock(13).unwrap();
        assert_eq!(locked.epoch, 13);
        assert_eq!(locked.phase, SleepPhase::Locked);
        assert!(transaction.abort_unlock(13).is_err());

        transaction.begin_unlock(13).unwrap();
        transaction.finish_unlock(13).unwrap();
    }

    #[test]
    fn every_awake_lock_interval_invalidates_the_previous_resuspend_timer() {
        let transaction = SleepTransaction::default();
        transaction.begin(17).unwrap();
        let locked = transaction.mark_locked(17).unwrap();
        let first_awake = transaction.mark_awake(17).unwrap();
        let second_awake = transaction.mark_awake(17).unwrap();

        assert_eq!(first_awake.epoch, 17);
        assert_eq!(first_awake.phase, SleepPhase::Locked);
        assert!(locked.revision < first_awake.revision);
        assert!(first_awake.revision < second_awake.revision);
        assert!(transaction.mark_awake(16).is_err());
    }

    #[test]
    fn reset_invalidates_queued_supervision_snapshots() {
        let transaction = SleepTransaction::default();
        transaction.begin(11).unwrap();
        let before = transaction.mark_locked(11).unwrap();
        let reset = transaction.reset().unwrap();
        assert_ne!(before.revision, reset.revision);
        assert_eq!(reset, transaction.snapshot());
    }

    #[test]
    fn display_recovery_requires_both_domain_and_sleep_fences() {
        assert!(recovery_fence_matches(4, 9, 4, 9));
        assert!(!recovery_fence_matches(5, 9, 4, 9));
        assert!(!recovery_fence_matches(4, 10, 4, 9));
    }

    #[test]
    fn auto_resuspend_loses_to_every_newer_lock_or_user_interaction() {
        let current = SleepSnapshot {
            epoch: 19,
            phase: SleepPhase::Locked,
            revision: 7,
        };
        assert!(resuspend_fence_matches(current, 19, 7, 11, 11));
        assert!(!resuspend_fence_matches(current, 18, 7, 11, 11));
        assert!(!resuspend_fence_matches(current, 19, 6, 11, 11));
        assert!(!resuspend_fence_matches(current, 19, 7, 12, 11));
        assert!(!resuspend_fence_matches(
            SleepSnapshot {
                phase: SleepPhase::Unlocking,
                ..current
            },
            19,
            7,
            11,
            11
        ));
    }
}
