mod applications;
mod control_v2;
mod input_mode;
mod launch;
mod navigation;
mod packages;
mod power;
mod request;
mod server;
mod sleep;
#[cfg(test)]
#[allow(dead_code)]
mod supervision;
mod sync;
mod utils;

use crate::{power_device, power_manager::PowerManager, system::SystemController};
use remagic_core::{AppId, AppSession, ManagerState, ManifestStore, SessionStore};
use remagic_protocol::PackageOperation;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};

pub(super) const MANIFEST_ROOT: &str = "/home/root/.local/share/remagic/apps.d";
pub(super) const SESSION_ROOT: &str = "/home/root/.local/state/remagic/sessions";
pub(super) const RUNTIME_ROOT: &str = "/run/remagic";
pub(super) const DISPLAY_UNIT: &str = "remagic-display-host.service";
pub(super) const HOME_UNIT: &str = "remagic-home.service";
pub(super) const FOREGROUND_MARKER: &str = "/run/remagic/foreground-app";
pub(super) const APP_REQUEST_SOCKET: &str = "/run/remagic/runtime-app.sock";
pub(super) const ACTIVITY_SOCKET: &str = "/run/remagic/activity.sock";
pub(super) const HOME_EVENT_SOCKET: &str = "/run/remagic/home-events.sock";

#[derive(Debug)]
pub(super) enum Event {
    UserActivity,
    SinglePower,
    TriplePower,
    LongPower,
    Launch(AppId, Option<PathBuf>),
    OpenManager,
    #[cfg(test)]
    EnsureManager,
    ReturnSystem,
    Sleep(u64),
    Wake(u64),
    Resuspend {
        sleep_epoch: u64,
        sleep_revision: u64,
        interaction_epoch: u64,
    },
    AutoSleep {
        activity_revision: u64,
    },
    Close(AppId, bool),
    RuntimeExited {
        app_id: AppId,
        generation: u64,
        exit_code: i32,
        crashed: bool,
        source: ExitReportSource,
    },
    #[allow(dead_code)]
    DisplayHostExited {
        state_sequence: u64,
        sleep_revision: u64,
    },
    AppReady(AppId),
    AppParked(AppSession),
    Package(PackageOperation),
    ReloadManifests,
}

impl Event {
    /// User-visible transitions supersede a cold launch which has not reached
    /// readiness yet. The event carries the epoch it created, so it never
    /// cancels its own `SinglePower -> launch(last_app)` route.
    fn interrupts_launch(&self) -> bool {
        matches!(
            self,
            Self::SinglePower
                | Self::TriplePower
                | Self::LongPower
                | Self::Launch(_, _)
                | Self::OpenManager
                | Self::ReturnSystem
                | Self::Sleep(_)
                | Self::Wake(_)
                | Self::Close(_, _)
                | Self::DisplayHostExited { .. }
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExitReportSource {
    Runner,
    #[cfg_attr(not(test), allow(dead_code))]
    Synthetic,
    Controlled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PendingExit {
    pub(super) generation: u64,
    pub(super) source: ExitReportSource,
}

pub(super) struct QueuedEvent {
    pub(super) event: Event,
    pub(super) reply: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    /// Set when the requester has stopped waiting. An event which has not
    /// begun must then have no delayed side effect.
    pub(super) request_fence: Arc<RequestFence>,
    /// Epoch after accounting for this event's own interrupt intent.
    pub(super) interrupt_epoch: u64,
}

const REQUEST_PENDING: u8 = 0;
const REQUEST_CANCELLED: u8 = 1;
const REQUEST_COMMITTING: u8 = 2;

pub(super) struct RequestFence(AtomicU8);

impl RequestFence {
    fn pending() -> Self {
        Self(AtomicU8::new(REQUEST_PENDING))
    }

    pub(super) fn cancel(&self) -> bool {
        self.0
            .compare_exchange(
                REQUEST_PENDING,
                REQUEST_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn begin_commit(&self) -> bool {
        self.0
            .compare_exchange(
                REQUEST_PENDING,
                REQUEST_COMMITTING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire) == REQUEST_CANCELLED
    }

    pub(super) fn is_committing(&self) -> bool {
        self.0.load(Ordering::Acquire) == REQUEST_COMMITTING
    }
}

impl QueuedEvent {
    pub(super) fn new(
        event: Event,
        reply: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
        request_fence: Arc<RequestFence>,
        launch_interrupt_epoch: &AtomicU64,
    ) -> Self {
        let interrupt_epoch = if event.interrupts_launch() {
            launch_interrupt_epoch
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1)
        } else {
            launch_interrupt_epoch.load(Ordering::Acquire)
        };
        Self {
            event,
            reply,
            request_fence,
            interrupt_epoch,
        }
    }

    pub(super) fn unattended(event: Event, launch_interrupt_epoch: &AtomicU64) -> Self {
        Self::non_cancellable(event, None, launch_interrupt_epoch)
    }

    pub(super) fn non_cancellable(
        event: Event,
        reply: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
        launch_interrupt_epoch: &AtomicU64,
    ) -> Self {
        Self::new(
            event,
            reply,
            Arc::new(RequestFence::pending()),
            launch_interrupt_epoch,
        )
    }
}

pub(super) struct Daemon {
    pub(super) state: RwLock<ManagerState>,
    pub(super) manifests: RwLock<BTreeMap<AppId, remagic_core::AppManifest>>,
    pub(super) sessions: RwLock<BTreeMap<AppId, AppSession>>,
    pub(super) runtime_generations: RwLock<BTreeMap<AppId, u64>>,
    /// Scheduling policy captured for the running process generation. A
    /// manifest reload must not change freezer semantics under a live cgroup.
    pub(super) runtime_background_execution:
        RwLock<BTreeMap<AppId, remagic_core::BackgroundExecution>>,
    pub(super) runtime_foreground_fences: RwLock<BTreeMap<AppId, (u64, u64)>>,
    /// The mode requested by an application for one exact foreground fence.
    /// Launching applications may publish this before the manager commits
    /// their surface, avoiding a ready/input-mode handshake cycle.
    pub(super) runtime_input_modes: RwLock<BTreeMap<AppId, input_mode::RuntimeInputState>>,
    pub(super) runtime_exit_reports: RwLock<BTreeMap<AppId, PendingExit>>,
    pub(super) runtime_missing_observations: RwLock<BTreeMap<AppId, (u64, u8)>>,
    pub(super) session_store: SessionStore,
    pub(super) manifest_store: ManifestStore,
    pub(super) controller: SystemController,
    pub(super) power: Arc<PowerManager>,
    pub(super) transition_lock: Mutex<()>,
    pub(super) events: mpsc::Sender<QueuedEvent>,
    pub(super) power_control: power_device::ControlSender,
    pub(super) next_generation: AtomicU64,
    pub(super) next_foreground_epoch: AtomicU64,
    /// Monotonic identity for display/power sleep transactions. Zero is
    /// reserved for "not locked" in both daemon and display telemetry.
    pub(super) next_sleep_epoch: AtomicU64,
    sleep_transaction: sleep::SleepTransaction,
    pub(super) launch_interrupt_epoch: Arc<AtomicU64>,
    #[cfg(test)]
    pub(super) manager_repair_pending: AtomicBool,
    pub(super) domain_recovery_pending: AtomicBool,
}

pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
    server::run().await
}

#[cfg(test)]
mod queue_tests {
    use super::*;

    #[test]
    fn a_new_user_interaction_supersedes_only_older_launch_epochs() {
        let epoch = AtomicU64::new(7);
        let first = QueuedEvent::unattended(
            Event::Launch(AppId::new("magicpaper").unwrap(), None),
            &epoch,
        );
        assert_eq!(first.interrupt_epoch, 8);
        assert_eq!(epoch.load(Ordering::Acquire), first.interrupt_epoch);

        let status = QueuedEvent::unattended(Event::EnsureManager, &epoch);
        assert_eq!(status.interrupt_epoch, first.interrupt_epoch);

        let close = QueuedEvent::unattended(
            Event::Close(AppId::new("magicpaper").unwrap(), false),
            &epoch,
        );
        assert!(close.interrupt_epoch > first.interrupt_epoch);
    }

    #[test]
    fn timed_out_queue_item_retains_a_shared_cancellation_fence() {
        let epoch = AtomicU64::new(1);
        let fence = Arc::new(RequestFence::pending());
        let queued = QueuedEvent::new(Event::EnsureManager, None, fence.clone(), &epoch);
        assert!(fence.cancel());
        assert!(queued.request_fence.is_cancelled());
    }

    #[test]
    fn timing_out_an_old_launch_does_not_cancel_a_newer_interaction_epoch() {
        let epoch = AtomicU64::new(1);
        let old = QueuedEvent::unattended(
            Event::Launch(AppId::new("magicpaper").unwrap(), None),
            &epoch,
        );
        let newer =
            QueuedEvent::unattended(Event::Launch(AppId::new("koreader").unwrap(), None), &epoch);
        assert!(old.request_fence.cancel());
        assert_eq!(epoch.load(Ordering::Acquire), newer.interrupt_epoch);
        assert!(!newer.request_fence.is_cancelled());
    }

    #[test]
    fn internal_runtime_evidence_is_constructed_without_a_cancellation_handle() {
        let epoch = AtomicU64::new(1);
        let exit = QueuedEvent::non_cancellable(
            Event::RuntimeExited {
                app_id: AppId::new("magicpaper").unwrap(),
                generation: 9,
                exit_code: 0,
                crashed: false,
                source: ExitReportSource::Runner,
            },
            None,
            &epoch,
        );
        assert!(!exit.request_fence.is_cancelled());
    }

    #[test]
    fn long_power_preempts_a_cold_launch() {
        let epoch = AtomicU64::new(3);
        let launch = QueuedEvent::unattended(
            Event::Launch(AppId::new("magicpaper").unwrap(), None),
            &epoch,
        );
        let long = QueuedEvent::unattended(Event::LongPower, &epoch);
        assert!(long.interrupt_epoch > launch.interrupt_epoch);
    }

    #[test]
    fn cancellation_and_foreground_commit_are_linearized() {
        let cancelled = RequestFence::pending();
        assert!(cancelled.cancel());
        assert!(!cancelled.begin_commit());

        let committing = RequestFence::pending();
        assert!(committing.begin_commit());
        assert!(!committing.cancel());
        assert!(committing.is_committing());
    }
}
