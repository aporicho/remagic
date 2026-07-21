use crate::geometry::Rect;
use crate::input::PenFrame;
use crate::surface::SharedSurface;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
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
    next_submission_sequence: AtomicU64,
    recent_submissions: Mutex<VecDeque<SubmissionRecord>>,
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

    fn mark_presented(&self, key: i32, sequence: u64) {
        *self.last_presented.lock().unwrap() = Some((key, sequence));
    }

    pub(crate) fn commit_foreground(&self, lease: PanelLease) {
        *self.committed_foreground.lock().unwrap() = Some(lease);
    }

    pub(crate) fn clear_committed_foreground(&self, lease: PanelLease) {
        let mut committed = self.committed_foreground.lock().unwrap();
        if committed.as_ref() == Some(&lease) {
            *committed = None;
        }
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
