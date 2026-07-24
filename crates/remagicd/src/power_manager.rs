//! Authoritative ReMagic power policy.
//!
//! Linux wake locks, RTC alarms and process freezing remain mechanisms owned
//! by the platform.  This module owns the policy clock and finite work leases
//! which decide when those mechanisms may be used.

use crate::daemon::{Event, QueuedEvent};
use remagic_core::{
    AppId, PowerPhase, PowerSettings, PowerSnapshot, PresentationState, ResourceLease, WorkClass,
    WorkloadState,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, Mutex, Notify, RwLock};
use tracing::warn;

const DEFAULT_CONFIG: &str = "/home/root/.config/remagic/power.toml";

mod settings;

#[derive(Clone, Debug)]
struct LeaseRecord {
    public: ResourceLease,
    expires_at: Instant,
}

#[derive(Debug)]
struct RuntimeState {
    phase: PowerPhase,
    presentation: PresentationState,
    workload: WorkloadState,
    last_activity: Instant,
    last_activity_unix_ms: u64,
    leases: BTreeMap<u64, LeaseRecord>,
    next_wake_unix_ms: Option<u64>,
    last_wake_reason: Option<String>,
    external_blocker: Option<String>,
}

pub struct PowerManager {
    config_path: PathBuf,
    settings: RwLock<PowerSettings>,
    runtime: Mutex<RuntimeState>,
    managed: AtomicBool,
    activity_revision: AtomicU64,
    next_lease: AtomicU64,
    changed: Notify,
}

impl PowerManager {
    pub fn load() -> Self {
        let config_path = std::env::var_os("REMAGIC_POWER_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));
        let settings = settings::load(&config_path).unwrap_or_else(|error| {
            warn!(%error, path = %config_path.display(), "power settings ignored");
            PowerSettings::default()
        });
        let now = Instant::now();
        Self {
            config_path,
            settings: RwLock::new(settings),
            runtime: Mutex::new(RuntimeState {
                phase: PowerPhase::Awake,
                presentation: PresentationState::None,
                workload: WorkloadState::Idle,
                last_activity: now,
                last_activity_unix_ms: unix_ms(),
                leases: BTreeMap::new(),
                next_wake_unix_ms: None,
                last_wake_reason: None,
                external_blocker: None,
            }),
            managed: AtomicBool::new(false),
            activity_revision: AtomicU64::new(1),
            next_lease: AtomicU64::new(1),
            changed: Notify::new(),
        }
    }

    pub fn spawn(
        self: &Arc<Self>,
        events: mpsc::Sender<QueuedEvent>,
        launch_interrupt_epoch: Arc<AtomicU64>,
    ) {
        let manager = Arc::clone(self);
        tokio::spawn(async move {
            manager.idle_loop(events, launch_interrupt_epoch).await;
        });
    }

    async fn idle_loop(
        &self,
        events: mpsc::Sender<QueuedEvent>,
        launch_interrupt_epoch: Arc<AtomicU64>,
    ) {
        loop {
            let Some((deadline, revision)) = self.next_policy_deadline().await else {
                self.changed.notified().await;
                continue;
            };
            tokio::select! {
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    if !self.reserve_auto_sleep(revision).await {
                        continue;
                    }
                    if events.send(QueuedEvent::unattended(
                        Event::AutoSleep { activity_revision: revision },
                        &launch_interrupt_epoch,
                    )).await.is_err() {
                        self.cancel_quiescing("manager event loop unavailable").await;
                        break;
                    }
                }
                _ = self.changed.notified() => {}
            }
        }
    }

    async fn next_policy_deadline(&self) -> Option<(Instant, u64)> {
        if !self.managed.load(Ordering::Acquire) {
            return None;
        }
        // USB power deliberately holds `udev.charger`. Starting a lock/suspend
        // transaction while any external blocker is present can only fail and
        // may steal the foreground during deployment. After unplugging, the
        // first real input event starts a fresh idle interval and wakes this
        // policy loop without adding a charger polling timer.
        if suspend_blocked_by_external_wake_lock(&read_wake_locks()) {
            return None;
        }
        let settings = self.settings.read().await.clone();
        if settings.idle_suspend_secs == 0 {
            return None;
        }
        let mut runtime = self.runtime.lock().await;
        prune_expired(&mut runtime);
        if runtime.phase != PowerPhase::Awake {
            return None;
        }
        let idle = runtime.last_activity + Duration::from_secs(settings.idle_suspend_secs);
        let deadline = runtime
            .leases
            .values()
            .map(|lease| lease.expires_at)
            .fold(idle, std::cmp::max);
        Some((deadline, self.activity_revision.load(Ordering::Acquire)))
    }

    async fn reserve_auto_sleep(&self, revision: u64) -> bool {
        if !self.managed.load(Ordering::Acquire)
            || self.activity_revision.load(Ordering::Acquire) != revision
            || suspend_blocked_by_external_wake_lock(&read_wake_locks())
        {
            return false;
        }
        let settings = self.settings.read().await.clone();
        if settings.idle_suspend_secs == 0 {
            return false;
        }
        let mut runtime = self.runtime.lock().await;
        prune_expired(&mut runtime);
        if runtime.phase != PowerPhase::Awake || !runtime.leases.is_empty() {
            return false;
        }
        if runtime.last_activity.elapsed() < Duration::from_secs(settings.idle_suspend_secs) {
            return false;
        }
        runtime.phase = PowerPhase::Quiescing;
        runtime.workload = WorkloadState::Idle;
        true
    }

    pub async fn enter_managed(&self) {
        self.managed.store(true, Ordering::Release);
        self.activity_revision.fetch_add(1, Ordering::AcqRel);
        let mut runtime = self.runtime.lock().await;
        runtime.phase = PowerPhase::Awake;
        runtime.presentation = PresentationState::Home;
        runtime.workload = WorkloadState::Idle;
        runtime.last_activity = Instant::now();
        runtime.last_activity_unix_ms = unix_ms();
        runtime.leases.clear();
        runtime.external_blocker = None;
        drop(runtime);
        self.changed.notify_one();
    }

    pub async fn enter_stock(&self) {
        self.managed.store(false, Ordering::Release);
        self.activity_revision.fetch_add(1, Ordering::AcqRel);
        let mut runtime = self.runtime.lock().await;
        runtime.phase = PowerPhase::Awake;
        runtime.presentation = PresentationState::None;
        runtime.workload = WorkloadState::Idle;
        runtime.leases.clear();
        runtime.next_wake_unix_ms = None;
        runtime.external_blocker = None;
        drop(runtime);
        self.changed.notify_one();
    }

    pub async fn note_activity(&self) {
        if !self.managed.load(Ordering::Acquire) {
            return;
        }
        self.activity_revision.fetch_add(1, Ordering::AcqRel);
        let mut runtime = self.runtime.lock().await;
        if matches!(
            runtime.phase,
            PowerPhase::Awake | PowerPhase::Quiescing | PowerPhase::Resuming
        ) {
            runtime.phase = PowerPhase::Awake;
            runtime.workload = WorkloadState::Interactive;
            runtime.last_activity = Instant::now();
            runtime.last_activity_unix_ms = unix_ms();
        }
        drop(runtime);
        self.changed.notify_one();
    }

    pub async fn auto_sleep_is_current(&self, revision: u64) -> bool {
        self.managed.load(Ordering::Acquire)
            && self.activity_revision.load(Ordering::Acquire) == revision
            && self.runtime.lock().await.phase == PowerPhase::Quiescing
    }

    pub async fn cancel_quiescing(&self, reason: &str) {
        let mut runtime = self.runtime.lock().await;
        if runtime.phase == PowerPhase::Quiescing {
            runtime.phase = PowerPhase::Awake;
            runtime.last_activity = Instant::now();
            runtime.last_activity_unix_ms = unix_ms();
            runtime.last_wake_reason = Some(reason.into());
        }
        drop(runtime);
        self.activity_revision.fetch_add(1, Ordering::AcqRel);
        self.changed.notify_one();
    }

    pub async fn begin_suspend(&self) {
        let mut runtime = self.runtime.lock().await;
        runtime.phase = PowerPhase::Quiescing;
        runtime.presentation = PresentationState::Lock;
        runtime.workload = WorkloadState::Idle;
        drop(runtime);
        self.changed.notify_one();
    }

    pub async fn suspended(&self) {
        let mut runtime = self.runtime.lock().await;
        runtime.phase = PowerPhase::Suspended;
        runtime.presentation = PresentationState::Lock;
        runtime.workload = WorkloadState::Idle;
        drop(runtime);
        self.changed.notify_one();
    }

    pub async fn externally_blocked(&self, reason: &str) {
        let mut runtime = self.runtime.lock().await;
        runtime.phase = PowerPhase::ExternallyBlocked;
        runtime.presentation = PresentationState::Lock;
        runtime.external_blocker = Some(classify_external_blocker(reason));
        drop(runtime);
        self.changed.notify_one();
    }

    pub async fn resumed(&self, reason: &str) {
        self.activity_revision.fetch_add(1, Ordering::AcqRel);
        let mut runtime = self.runtime.lock().await;
        runtime.phase = PowerPhase::Awake;
        runtime.presentation = PresentationState::Home;
        runtime.workload = WorkloadState::Interactive;
        runtime.last_activity = Instant::now();
        runtime.last_activity_unix_ms = unix_ms();
        runtime.last_wake_reason = Some(reason.into());
        runtime.external_blocker = None;
        drop(runtime);
        self.changed.notify_one();
    }

    pub async fn set_presentation(&self, presentation: PresentationState) {
        self.runtime.lock().await.presentation = presentation;
    }

    pub async fn begin_work(
        &self,
        owner: AppId,
        class: WorkClass,
        reason: impl Into<String>,
        requested_ms: u64,
    ) -> ResourceLease {
        let ttl_ms = requested_ms.clamp(1, class.maximum_lease_ms());
        let id = self.next_lease.fetch_add(1, Ordering::Relaxed).max(1);
        let public = ResourceLease {
            id,
            owner,
            class,
            reason: reason.into(),
            expires_at_unix_ms: unix_ms().saturating_add(ttl_ms),
        };
        let mut runtime = self.runtime.lock().await;
        runtime.workload = match class {
            WorkClass::PackageTransaction | WorkClass::FileTransfer => WorkloadState::Maintenance,
            WorkClass::AgentTurn => WorkloadState::ScheduledJob,
            _ => WorkloadState::Interactive,
        };
        if runtime.phase == PowerPhase::Quiescing {
            runtime.phase = PowerPhase::Awake;
        }
        runtime.leases.insert(
            id,
            LeaseRecord {
                public: public.clone(),
                expires_at: Instant::now() + Duration::from_millis(ttl_ms),
            },
        );
        drop(runtime);
        self.activity_revision.fetch_add(1, Ordering::AcqRel);
        self.changed.notify_one();
        public
    }

    pub async fn finish_work(&self, owner: &AppId, lease_id: u64, visible_result: bool) -> bool {
        let mut runtime = self.runtime.lock().await;
        let removed = runtime
            .leases
            .get(&lease_id)
            .is_some_and(|lease| &lease.public.owner == owner)
            && runtime.leases.remove(&lease_id).is_some();
        if removed && runtime.leases.is_empty() {
            runtime.workload = WorkloadState::Idle;
            if visible_result {
                runtime.last_activity = Instant::now();
                runtime.last_activity_unix_ms = unix_ms();
            }
        }
        drop(runtime);
        if visible_result {
            self.activity_revision.fetch_add(1, Ordering::AcqRel);
        }
        self.changed.notify_one();
        removed
    }

    pub async fn set_idle_suspend(&self, seconds: u64) -> Result<PowerSettings, String> {
        let settings = PowerSettings {
            idle_suspend_secs: seconds,
            ..PowerSettings::default()
        };
        settings.validate()?;
        settings::save(&self.config_path, &settings)?;
        *self.settings.write().await = settings.clone();
        self.activity_revision.fetch_add(1, Ordering::AcqRel);
        let mut runtime = self.runtime.lock().await;
        runtime.last_activity = Instant::now();
        runtime.last_activity_unix_ms = unix_ms();
        drop(runtime);
        self.changed.notify_one();
        Ok(settings)
    }

    pub async fn snapshot(&self) -> PowerSnapshot {
        let settings = self.settings.read().await.clone();
        let mut runtime = self.runtime.lock().await;
        prune_expired(&mut runtime);
        let wake_lock_owners = read_wake_locks();
        let idle_deadline_unix_ms = if self.managed.load(Ordering::Acquire)
            && runtime.phase == PowerPhase::Awake
            && settings.idle_suspend_secs != 0
            && !suspend_blocked_by_external_wake_lock(&wake_lock_owners)
        {
            let idle = runtime
                .last_activity_unix_ms
                .saturating_add(settings.idle_suspend_secs.saturating_mul(1_000));
            Some(
                runtime
                    .leases
                    .values()
                    .map(|lease| lease.public.expires_at_unix_ms)
                    .fold(idle, std::cmp::max),
            )
        } else {
            None
        };
        PowerSnapshot {
            phase: runtime.phase,
            presentation: runtime.presentation.clone(),
            workload: runtime.workload,
            idle_suspend_secs: settings.idle_suspend_secs,
            idle_deadline_unix_ms,
            wake_lock_owners,
            active_leases: runtime
                .leases
                .values()
                .map(|lease| lease.public.clone())
                .collect(),
            next_wake_unix_ms: runtime.next_wake_unix_ms,
            suspend_successes: read_suspend_successes(),
            last_wake_reason: runtime.last_wake_reason.clone(),
            external_blocker: runtime.external_blocker.clone(),
        }
    }
}

fn prune_expired(runtime: &mut RuntimeState) {
    let now = Instant::now();
    runtime.leases.retain(|_, lease| lease.expires_at > now);
    if runtime.leases.is_empty()
        && matches!(
            runtime.workload,
            WorkloadState::ScheduledJob | WorkloadState::Maintenance
        )
    {
        runtime.workload = WorkloadState::Idle;
    }
}

fn classify_external_blocker(reason: &str) -> String {
    if reason.split_whitespace().any(|owner| {
        owner.trim_matches(|character: char| !character.is_ascii_alphanumeric() && character != '.')
            == "udev.charger"
    }) {
        "charger".into()
    } else {
        reason.into()
    }
}

fn read_wake_locks() -> Vec<String> {
    fs::read_to_string("/sys/power/wake_lock")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn suspend_blocked_by_external_wake_lock(owners: &[String]) -> bool {
    owners
        .iter()
        .any(|owner| owner.as_str() != "remagic-managed")
}

fn read_suspend_successes() -> u64 {
    fs::read_to_string("/sys/power/suspend_stats/success")
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests;
