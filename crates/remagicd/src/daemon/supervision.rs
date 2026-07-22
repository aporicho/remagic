use super::*;
use crate::display_host;
use remagic_core::DomainState;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tracing::warn;

const SUPERVISION_INTERVAL: std::time::Duration = std::time::Duration::from_millis(750);

pub(super) fn spawn(daemon: Arc<Daemon>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SUPERVISION_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if supervise_once(&daemon).await.is_err() {
                break;
            }
        }
    });
}

async fn supervise_once(daemon: &Daemon) -> Result<(), ()> {
    let domain = daemon.state.read().await.domain.clone();
    if matches!(
        domain,
        DomainState::System | DomainState::EnteringManaged | DomainState::RestoringSystem
    ) {
        return Ok(());
    }
    if !daemon.controller.is_active(DISPLAY_UNIT).await {
        return queue_display_recovery(daemon).await;
    }
    queue_missing_runtimes(daemon).await?;
    if matches!(domain, DomainState::Manager) && !manager_surface_is_healthy(daemon).await {
        queue_manager_repair(daemon).await?;
    }
    let sleep = daemon.sleep_transaction.snapshot();
    if matches!(domain, DomainState::Sleeping)
        && sleep.phase == sleep::SleepPhase::Locked
        && !lock_surface_is_healthy(sleep).await
    {
        warn!("display lock lost its committed presentation; restoring stock shell");
        return queue_display_recovery(daemon).await;
    }
    Ok(())
}

async fn queue_display_recovery(daemon: &Daemon) -> Result<(), ()> {
    if daemon
        .domain_recovery_pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }
    let state_sequence = daemon.state.read().await.sequence;
    let sleep_revision = daemon.sleep_transaction.snapshot().revision;
    warn!(
        state_sequence,
        sleep_revision,
        "display host disappeared while managed domain was active; restoring stock shell"
    );
    if daemon
        .events
        .send(QueuedEvent::unattended(
            Event::DisplayHostExited {
                state_sequence,
                sleep_revision,
            },
            &daemon.launch_interrupt_epoch,
        ))
        .await
        .is_err()
    {
        daemon
            .domain_recovery_pending
            .store(false, Ordering::Release);
        return Err(());
    }
    Ok(())
}

async fn queue_missing_runtimes(daemon: &Daemon) -> Result<(), ()> {
    let tracked = daemon.runtime_generations.read().await.clone();
    let inactive = inactive_runtimes(&daemon.controller, &tracked).await;
    retain_current_missing_observations(daemon, &inactive).await;
    for (app_id, generation) in inactive {
        if !confirm_missing_runtime(daemon, &app_id, generation).await {
            continue;
        }
        let pending = PendingExit {
            generation,
            source: ExitReportSource::Synthetic,
        };
        let should_report = {
            let mut reports = daemon.runtime_exit_reports.write().await;
            reserve_synthetic_report(&mut reports, app_id.clone(), pending)
        };
        if !should_report {
            continue;
        }
        warn!(%app_id, generation, "managed application cgroup disappeared");
        if daemon
            .events
            .send(QueuedEvent::unattended(
                Event::RuntimeExited {
                    app_id: app_id.clone(),
                    generation,
                    exit_code: -1,
                    crashed: true,
                    source: ExitReportSource::Synthetic,
                },
                &daemon.launch_interrupt_epoch,
            ))
            .await
            .is_err()
        {
            let mut reports = daemon.runtime_exit_reports.write().await;
            if reports.get(&app_id) == Some(&pending) {
                reports.remove(&app_id);
            }
            return Err(());
        }
    }
    Ok(())
}

fn reserve_synthetic_report(
    reports: &mut BTreeMap<AppId, PendingExit>,
    app_id: AppId,
    pending: PendingExit,
) -> bool {
    if reports
        .get(&app_id)
        .is_some_and(|report| report.generation == pending.generation)
    {
        return false;
    }
    reports.insert(app_id, pending);
    true
}

async fn retain_current_missing_observations(daemon: &Daemon, inactive: &[(AppId, u64)]) {
    daemon
        .runtime_missing_observations
        .write()
        .await
        .retain(|app_id, (generation, _)| {
            inactive.iter().any(|(missing_id, missing_generation)| {
                missing_id == app_id && missing_generation == generation
            })
        });
}

async fn confirm_missing_runtime(daemon: &Daemon, app_id: &AppId, generation: u64) -> bool {
    let mut observations = daemon.runtime_missing_observations.write().await;
    match observations.get_mut(app_id) {
        Some((seen_generation, count)) if *seen_generation == generation => {
            *count = count.saturating_add(1);
            *count >= 2
        }
        _ => {
            observations.insert(app_id.clone(), (generation, 1));
            false
        }
    }
}

async fn manager_surface_is_healthy(daemon: &Daemon) -> bool {
    if !daemon.controller.is_active(HOME_UNIT).await {
        return false;
    }
    display_host::status().await.is_ok_and(|snapshot| {
        snapshot.foreground_key == Some(display_host::HOME_SURFACE_KEY)
            && snapshot
                .surface_sequences
                .get(&display_host::HOME_SURFACE_KEY)
                .copied()
                .unwrap_or(0)
                > 0
    })
}

async fn lock_surface_is_healthy(sleep: sleep::SleepSnapshot) -> bool {
    if sleep.epoch == 0 || sleep.phase != sleep::SleepPhase::Locked {
        return false;
    }
    // Once committed, the lock image lives in display-host's private panel
    // buffer. Home is Restart=on-failure and may briefly disconnect/recreate
    // its writable QTFB surface without making those frozen pixels unsafe.
    display_host::status()
        .await
        .is_ok_and(|snapshot| snapshot_has_committed_lock(&snapshot, sleep.epoch))
}

fn snapshot_has_committed_lock(snapshot: &display_host::Snapshot, sleep_epoch: u64) -> bool {
    sleep_epoch != 0
        && snapshot.lock_epoch == sleep_epoch
        && snapshot.lock_committed
        && snapshot.foreground_key == Some(display_host::HOME_SURFACE_KEY)
}

async fn queue_manager_repair(daemon: &Daemon) -> Result<(), ()> {
    if daemon
        .manager_repair_pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(());
    }
    if daemon
        .events
        .send(QueuedEvent::unattended(
            Event::EnsureManager,
            &daemon.launch_interrupt_epoch,
        ))
        .await
        .is_err()
    {
        daemon
            .manager_repair_pending
            .store(false, Ordering::Release);
        return Err(());
    }
    Ok(())
}

trait UnitProbe {
    fn active<'a>(&'a self, unit: &'a str) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;
}

impl UnitProbe for SystemController {
    fn active<'a>(&'a self, unit: &'a str) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(self.is_active(unit))
    }
}

async fn inactive_runtimes<P: UnitProbe>(
    probe: &P,
    tracked: &BTreeMap<AppId, u64>,
) -> Vec<(AppId, u64)> {
    let mut missing = Vec::new();
    for (app_id, generation) in tracked {
        if !probe.active(&utils::app_unit(app_id)).await {
            missing.push((app_id.clone(), *generation));
        }
    }
    missing
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    struct FakeProbe(BTreeSet<String>);

    impl UnitProbe for FakeProbe {
        fn active<'a>(&'a self, unit: &'a str) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
            Box::pin(async move { self.0.contains(unit) })
        }
    }

    #[tokio::test]
    async fn reports_only_tracked_inactive_generations() {
        let magicpaper = AppId::new("magicpaper").unwrap();
        let koreader = AppId::new("koreader").unwrap();
        let tracked = BTreeMap::from([(magicpaper.clone(), 4), (koreader.clone(), 9)]);
        let probe = FakeProbe(BTreeSet::from([utils::app_unit(&magicpaper)]));
        assert_eq!(
            inactive_runtimes(&probe, &tracked).await,
            vec![(koreader, 9)]
        );
    }

    #[tokio::test]
    async fn no_false_exit_when_all_tracked_units_are_active() {
        let app = AppId::new("magicpaper").unwrap();
        let tracked = BTreeMap::from([(app.clone(), 4)]);
        let probe = FakeProbe(BTreeSet::from([utils::app_unit(&app)]));
        assert!(inactive_runtimes(&probe, &tracked).await.is_empty());
    }

    #[test]
    fn frozen_lock_remains_healthy_while_home_surface_reconnects() {
        let snapshot = display_host::Snapshot {
            foreground_key: Some(display_host::HOME_SURFACE_KEY),
            generation: 4,
            foreground_epoch: 8,
            lock_epoch: 17,
            lock_committed: true,
            // A crashed Home has no current writable surface; the committed
            // panel buffer is nevertheless still the authoritative lock.
            surfaces: Vec::new(),
            ..display_host::Snapshot::default()
        };
        assert!(snapshot_has_committed_lock(&snapshot, 17));
        assert!(!snapshot_has_committed_lock(&snapshot, 16));
    }

    #[test]
    fn runner_report_blocks_synthetic_crash_for_same_generation() {
        let app = AppId::new("magicpaper").unwrap();
        let runner = PendingExit {
            generation: 12,
            source: ExitReportSource::Runner,
        };
        let synthetic = PendingExit {
            generation: 12,
            source: ExitReportSource::Synthetic,
        };
        let mut reports = BTreeMap::from([(app.clone(), runner)]);
        assert!(!reserve_synthetic_report(
            &mut reports,
            app.clone(),
            synthetic
        ));
        assert_eq!(reports.get(&app), Some(&runner));
    }
}
