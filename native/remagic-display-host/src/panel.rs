use crate::geometry::Rect;
use crate::input::PenFrame;
use crate::surface::SharedSurface;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::io;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) const SUBMISSION_HISTORY_CAPACITY: usize = 64;

mod backend;
mod ink;
mod render;
mod runtime;

#[cfg(feature = "device")]
pub use backend::QuillBackend;
pub use backend::{MemoryBackend, MemorySubmission};
pub use runtime::PanelRuntime;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct PanelLease {
    pub key: i32,
    pub generation: u64,
    pub foreground_epoch: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionReason {
    ForegroundSwitch,
    SurfaceDamage,
    FullRefresh,
    LockScreen,
    LockRefresh,
    UnlockScreen,
    LiveInk,
    CanonicalSettle,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SubmissionRecord {
    pub sequence: u64,
    pub surface_sequence: u64,
    pub key: i32,
    pub generation: u64,
    pub foreground_epoch: u64,
    pub intent: RefreshIntent,
    pub reason: SubmissionReason,
    pub visible_signature: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<u64>,
    pub success: bool,
}

#[derive(Default)]
pub struct PanelTelemetry {
    submission_count: AtomicU64,
    last_marker: AtomicU64,
    failure_count: AtomicU64,
    visible_signature: AtomicU64,
    full_refresh_count: AtomicU64,
    queue_depth: AtomicUsize,
    last_presented: Mutex<Option<(i32, u64)>>,
    committed_foreground: Mutex<Option<PanelLease>>,
    committed_ink: Mutex<Option<(PanelLease, bool)>>,
    committed_lock_epoch: AtomicU64,
    cancelled_lock_epoch: AtomicU64,
    next_submission_sequence: AtomicU64,
    recent_submissions: Mutex<VecDeque<SubmissionRecord>>,
    deferred_damage: Mutex<HashMap<PanelLease, DeferredDamage>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DeferredDamage {
    pub(crate) rect: Rect,
    pub(crate) intent: RefreshIntent,
}

impl PanelTelemetry {
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.submission_count.load(Ordering::Acquire),
            self.last_marker.load(Ordering::Acquire),
            self.failure_count.load(Ordering::Acquire),
            self.visible_signature.load(Ordering::Acquire),
        )
    }

    pub fn mark_failure(&self) {
        self.failure_count.fetch_add(1, Ordering::AcqRel);
    }

    pub fn command_enqueued(&self) {
        self.queue_depth.fetch_add(1, Ordering::AcqRel);
    }

    pub fn command_dequeued(&self) {
        let _ = self
            .queue_depth
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |depth| {
                depth.checked_sub(1)
            });
    }

    pub fn queue_depth(&self) -> usize {
        self.queue_depth.load(Ordering::Acquire)
    }

    pub fn full_refresh_count(&self) -> u64 {
        self.full_refresh_count.load(Ordering::Acquire)
    }

    pub fn last_presented(&self) -> Option<(i32, u64)> {
        *self.last_presented.lock().unwrap()
    }

    pub fn recent_submissions(&self) -> Vec<SubmissionRecord> {
        self.recent_submissions
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect()
    }

    pub fn committed_foreground(&self) -> Option<PanelLease> {
        *self.committed_foreground.lock().unwrap()
    }

    pub fn committed_lock_epoch(&self) -> u64 {
        self.committed_lock_epoch.load(Ordering::Acquire)
    }

    pub fn cancelled_lock_epoch(&self) -> u64 {
        self.cancelled_lock_epoch.load(Ordering::Acquire)
    }

    pub fn committed_ink(&self) -> Option<(PanelLease, bool)> {
        *self.committed_ink.lock().unwrap()
    }

    pub(crate) fn defer_damage(&self, lease: PanelLease, rect: Rect, intent: RefreshIntent) {
        let mut pending = self.deferred_damage.lock().unwrap();
        pending
            .entry(lease)
            .and_modify(|damage| {
                damage.rect = damage.rect.union(rect);
                damage.intent = stronger_intent(damage.intent, intent);
            })
            .or_insert(DeferredDamage { rect, intent });
    }

    pub(crate) fn take_deferred_damage(&self, lease: PanelLease) -> Option<DeferredDamage> {
        self.deferred_damage.lock().unwrap().remove(&lease)
    }

    pub(crate) fn discard_deferred_damage(&self, lease: PanelLease) {
        self.deferred_damage.lock().unwrap().remove(&lease);
    }

    pub(crate) fn discard_deferred_damage_for_key(&self, key: i32) {
        self.deferred_damage
            .lock()
            .unwrap()
            .retain(|lease, _| lease.key != key);
    }

    fn mark_presented(&self, key: i32, sequence: u64) {
        *self.last_presented.lock().unwrap() = Some((key, sequence));
    }

    pub(crate) fn commit_foreground(&self, lease: PanelLease) {
        *self.committed_foreground.lock().unwrap() = Some(lease);
    }

    pub(crate) fn commit_ink(&self, lease: PanelLease, enabled: bool) {
        *self.committed_ink.lock().unwrap() = Some((lease, enabled));
    }

    pub(crate) fn clear_committed_foreground(&self, lease: PanelLease) {
        let mut committed = self.committed_foreground.lock().unwrap();
        if committed.as_ref() == Some(&lease) {
            *committed = None;
        }
        let mut ink = self.committed_ink.lock().unwrap();
        if ink.is_some_and(|(ink_lease, _)| ink_lease == lease) {
            *ink = None;
        }
    }

    pub(crate) fn commit_lock(&self, sleep_epoch: u64) {
        self.committed_lock_epoch
            .store(sleep_epoch, Ordering::Release);
    }

    pub(crate) fn clear_committed_lock(&self, sleep_epoch: u64) {
        let _ = self.committed_lock_epoch.compare_exchange(
            sleep_epoch,
            0,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(crate) fn record_lock_cancelled(&self, sleep_epoch: u64) {
        self.cancelled_lock_epoch
            .store(sleep_epoch, Ordering::Release);
    }

    fn mark_full_refresh(&self) {
        self.full_refresh_count.fetch_add(1, Ordering::AcqRel);
    }

    fn record_submission(&self, mut record: SubmissionRecord) {
        record.sequence = self
            .next_submission_sequence
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            .max(1);
        let mut history = self.recent_submissions.lock().unwrap();
        if history.len() == SUBMISSION_HISTORY_CAPACITY {
            history.pop_front();
        }
        history.push_back(record);
    }
}

fn stronger_intent(left: RefreshIntent, right: RefreshIntent) -> RefreshIntent {
    fn rank(intent: RefreshIntent) -> u8 {
        match intent {
            RefreshIntent::Ink => 0,
            RefreshIntent::MonoQuality => 1,
            RefreshIntent::Ui => 2,
            RefreshIntent::Content => 3,
            RefreshIntent::Full => 4,
        }
    }
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshIntent {
    Ink,
    MonoQuality,
    Ui,
    Content,
    Full,
}

#[derive(Clone)]
pub enum PanelCommand {
    RegisterSurface(Arc<SharedSurface>),
    DropSurface {
        key: i32,
    },
    Damage {
        lease: PanelLease,
        rect: Rect,
        intent: RefreshIntent,
    },
    SetForeground {
        lease: PanelLease,
        full_refresh: bool,
    },
    ActivateForeground {
        lease: PanelLease,
        ink_enabled: bool,
        full_refresh: bool,
    },
    ClearForeground {
        lease: PanelLease,
    },
    ConfigureInk {
        lease: PanelLease,
        enabled: bool,
        region: Option<Rect>,
    },
    Pen {
        lease: PanelLease,
        frame: PenFrame,
    },
    FullRefresh {
        lease: PanelLease,
    },
    ShowLock {
        lease: PanelLease,
        sleep_epoch: u64,
    },
    RefreshLock {
        lease: PanelLease,
        sleep_epoch: u64,
    },
    CancelLock {
        lease: PanelLease,
        sleep_epoch: u64,
        replacement_surface_sequence: u64,
    },
    Shutdown,
}

pub trait PanelBackend {
    fn width(&self) -> i32;
    fn height(&self) -> i32;
    fn stride(&self) -> usize;
    fn pixels_mut(&mut self) -> &mut [u8];
    fn submit(&mut self, rect: Rect, intent: RefreshIntent) -> io::Result<u64>;
    fn process_events(&mut self) {}
}
