use super::*;

#[test]
fn expired_leases_do_not_survive_policy_evaluation() {
    let now = Instant::now();
    let mut runtime = RuntimeState {
        phase: PowerPhase::Awake,
        presentation: PresentationState::Home,
        workload: WorkloadState::Maintenance,
        last_activity: now,
        last_activity_unix_ms: 0,
        leases: BTreeMap::from([(
            1,
            LeaseRecord {
                public: ResourceLease {
                    id: 1,
                    owner: AppId::new("test").unwrap(),
                    class: WorkClass::StorageCommit,
                    reason: "test".into(),
                    expires_at_unix_ms: 0,
                },
                expires_at: now.checked_sub(Duration::from_secs(1)).unwrap(),
            },
        )]),
        next_wake_unix_ms: None,
        last_wake_reason: None,
        external_blocker: None,
    };
    prune_expired(&mut runtime);
    assert!(runtime.leases.is_empty());
    assert_eq!(runtime.workload, WorkloadState::Idle);
}

#[test]
fn charger_wakelock_is_reported_as_an_expected_external_blocker() {
    assert_eq!(
        classify_external_blocker("kernel suspend is blocked by active wake locks: udev.charger"),
        "charger"
    );
    assert_eq!(classify_external_blocker("wifi"), "wifi");
}

#[test]
fn external_wake_locks_suppress_idle_suspend_but_our_own_lock_does_not() {
    assert!(suspend_blocked_by_external_wake_lock(&[
        "udev.charger".into(),
    ]));
    assert!(suspend_blocked_by_external_wake_lock(&[
        "remagic-managed".into(),
        "wifi".into(),
    ]));
    assert!(!suspend_blocked_by_external_wake_lock(&[]));
    assert!(!suspend_blocked_by_external_wake_lock(&[
        "remagic-managed".into(),
    ]));
}
