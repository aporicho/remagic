use super::*;
use crate::AppId;

fn id(value: &str) -> AppId {
    AppId::new(value).unwrap()
}

#[test]
fn system_manager_app_round_trip() {
    let mut state = ManagerState::default();
    state.apply(Transition::TriplePower).unwrap();
    state.apply(Transition::ManagedReady).unwrap();
    state.apply(Transition::Launch(id("koreader"))).unwrap();
    state.apply(Transition::AppReady(id("koreader"))).unwrap();
    state.apply(Transition::SinglePower).unwrap();
    state.apply(Transition::AppParked(id("koreader"))).unwrap();
    assert_eq!(state.domain, DomainState::Manager);
    assert_eq!(state.last_app, Some(id("koreader")));
    state.apply(Transition::SinglePower).unwrap();
    assert_eq!(state.domain, DomainState::Launching(id("koreader")));
}

#[test]
fn manager_triple_returns_system() {
    let mut state = ManagerState {
        domain: DomainState::Manager,
        ..ManagerState::default()
    };
    state.apply(Transition::TriplePower).unwrap();
    state.apply(Transition::SystemReady).unwrap();
    assert_eq!(state.domain, DomainState::System);
}

#[test]
fn sleep_remains_locked_until_an_explicit_wake_transition() {
    let mut state = ManagerState {
        domain: DomainState::Manager,
        sequence: 7,
        ..ManagerState::default()
    };
    state.apply(Transition::Sleep).unwrap();
    assert_eq!(state.domain, DomainState::Sleeping);
    assert!(state.apply(Transition::Sleep).is_err());
    assert_eq!(state.domain, DomainState::Sleeping);
    state.apply(Transition::Wake).unwrap();
    assert_eq!(state.domain, DomainState::Manager);
}

#[test]
fn foreground_crash_returns_to_manager() {
    let app = id("magicpaper");
    let mut state = ManagerState {
        domain: DomainState::Foreground(app.clone()),
        last_app: Some(app.clone()),
        sequence: 3,
    };
    state.apply(Transition::AppCrashed(app.clone())).unwrap();
    assert_eq!(state.domain, DomainState::Manager);
    assert_eq!(state.last_app, Some(app));
}

#[test]
fn foreground_normal_exit_returns_directly_to_manager() {
    let app = id("koreader");
    let mut state = ManagerState {
        domain: DomainState::Foreground(app.clone()),
        last_app: Some(app.clone()),
        sequence: 4,
    };
    state.apply(Transition::AppExited(app.clone())).unwrap();
    assert_eq!(state.domain, DomainState::Manager);
    assert_eq!(state.last_app, Some(app));
}

#[test]
fn failed_park_can_restore_foreground_and_rejects_its_late_ack() {
    let app = id("magicpaper");
    let mut state = ManagerState {
        domain: DomainState::Foreground(app.clone()),
        last_app: Some(app.clone()),
        sequence: 4,
    };
    state.apply(Transition::SinglePower).unwrap();
    assert_eq!(state.domain, DomainState::Parking(app.clone()));

    state.apply(Transition::AppRestored(app.clone())).unwrap();
    assert_eq!(state.domain, DomainState::Foreground(app.clone()));
    let restored_sequence = state.sequence;

    assert!(state.apply(Transition::AppParked(app.clone())).is_err());
    assert_eq!(state.domain, DomainState::Foreground(app.clone()));
    assert_eq!(state.sequence, restored_sequence);
    assert_eq!(state.last_app, Some(app));
}

fn managed_state() -> SupervisorState {
    let mut state = SupervisorState::default();
    state
        .transition_domain(SystemDomainState::EnteringManaged)
        .unwrap();
    state.transition_domain(SystemDomainState::Managed).unwrap();
    state
}

#[test]
fn v2_tracks_domain_and_multiple_app_instances_independently() {
    let magicpaper = id("magicpaper");
    let koreader = id("koreader");
    let mut state = managed_state();

    state.start_app(magicpaper.clone(), 1, Some(10)).unwrap();
    state.grant_foreground(&magicpaper, 1, 1, 100).unwrap();
    state.enter_background(&magicpaper, 1, 1).unwrap();
    state.start_app(koreader.clone(), 7, Some(20)).unwrap();
    state.grant_foreground(&koreader, 7, 1, 200).unwrap();

    assert_eq!(state.domain, SystemDomainState::Managed);
    assert_eq!(state.foreground_app, Some(koreader.clone()));
    assert_eq!(state.apps[&magicpaper].state, AppInstanceState::Background);
    assert_eq!(state.apps[&koreader].state, AppInstanceState::Foreground);
    assert!(state.validate().is_ok());

    state.enter_background(&koreader, 7, 1).unwrap();
    let token = state.grant_foreground(&magicpaper, 1, 2, 300).unwrap();
    assert_eq!(token.foreground_epoch, 2);
    assert_eq!(token.lease_id, Some(300));
    assert!(state.validate().is_ok());
}

#[test]
fn stale_generation_epoch_and_lease_are_rejected_without_revision_change() {
    let app = id("magicpaper");
    let mut state = managed_state();
    state.start_app(app.clone(), 4, None).unwrap();
    state.grant_foreground(&app, 4, 8, 9).unwrap();

    let revision = state.state_revision;
    assert!(matches!(
        state.enter_background(&app, 3, 8),
        Err(StateModelError::StaleGeneration { actual: 3, .. })
    ));
    assert_eq!(state.state_revision, revision);

    assert!(matches!(
        state.enter_background(&app, 4, 7),
        Err(StateModelError::StaleForegroundEpoch { actual: 7, .. })
    ));
    assert_eq!(state.state_revision, revision);

    state.enter_background(&app, 4, 8).unwrap();
    let revision = state.state_revision;
    assert!(matches!(
        state.grant_foreground(&app, 4, 8, 10),
        Err(StateModelError::StaleForegroundEpoch { actual: 8, .. })
    ));
    assert_eq!(state.state_revision, revision);
    assert!(matches!(
        state.grant_foreground(&app, 4, 9, 0),
        Err(StateModelError::ZeroLease(_))
    ));
    assert_eq!(state.state_revision, revision);
}

#[test]
fn headless_ready_and_unresponsive_are_explicit_app_states() {
    let app = id("indexer");
    let mut state = managed_state();
    state.start_app(app.clone(), 1, Some(9)).unwrap();
    state
        .mark_background_ready(&app, 1, "Indexer".into(), "Idle".into())
        .unwrap();
    assert_eq!(state.apps[&app].state, AppInstanceState::Background);
    assert_eq!(state.apps[&app].title, "Indexer");
    assert!(state.validate().is_ok());

    state
        .mark_unresponsive(&app, 1, "heartbeat timeout".into())
        .unwrap();
    assert_eq!(state.apps[&app].state, AppInstanceState::Unresponsive);
    assert_eq!(
        state.apps[&app].last_error.as_deref(),
        Some("heartbeat timeout")
    );
    assert!(state.validate().is_ok());

    state.begin_stop(&app, 1).unwrap();
    state.finish_app(&app, 1, false, None).unwrap();
    let revision = state.state_revision;
    assert!(matches!(
        state.finish_app(&app, 1, false, None),
        Err(StateModelError::InvalidAppTransition { .. })
    ));
    assert_eq!(state.state_revision, revision);
}

#[test]
fn state_revision_overflow_is_atomic() {
    let mut state = SupervisorState {
        state_revision: u64::MAX,
        ..SupervisorState::default()
    };
    let before = state.clone();
    assert_eq!(
        state.transition_domain(SystemDomainState::EnteringManaged),
        Err(StateModelError::RevisionOverflow)
    );
    assert_eq!(state, before);
}

#[test]
fn domain_transition_matrix_rejects_all_unlisted_edges() {
    use SystemDomainState::*;
    let states = [Stock, EnteringManaged, Managed, LeavingManaged, Recovering];
    let allowed = [
        (Stock, EnteringManaged),
        (EnteringManaged, Managed),
        (Managed, LeavingManaged),
        (LeavingManaged, Stock),
        (Recovering, Stock),
        (Stock, Recovering),
        (EnteringManaged, Recovering),
        (Managed, Recovering),
        (LeavingManaged, Recovering),
    ];
    for from in states {
        for to in states {
            let mut state = SupervisorState {
                domain: from,
                ..SupervisorState::default()
            };
            assert_eq!(
                state.transition_domain(to).is_ok(),
                allowed.contains(&(from, to)),
                "{from:?} -> {to:?}"
            );
        }
    }
}

#[test]
fn v1_composite_states_round_trip_through_v2_compatibility() {
    let app = id("koreader");
    for domain in [
        DomainState::System,
        DomainState::EnteringManaged,
        DomainState::Manager,
        DomainState::Launching(app.clone()),
        DomainState::Foreground(app.clone()),
        DomainState::Parking(app.clone()),
        DomainState::RestoringSystem,
        DomainState::Sleeping,
        DomainState::Recovering,
    ] {
        let legacy = ManagerState {
            domain,
            last_app: Some(app.clone()),
            sequence: 42,
        };
        let v2 = SupervisorState::from(legacy.clone());
        assert!(v2.validate().is_ok(), "{:?}", legacy.domain);
        assert_eq!(ManagerState::from(&v2), legacy);
    }
}

#[test]
fn malformed_foreground_snapshot_is_detected() {
    let mut state = managed_state();
    let a = id("app-a");
    let b = id("app-b");
    state.apps.insert(
        a.clone(),
        AppInstance {
            token: AppToken {
                app_id: a.clone(),
                generation: 1,
                foreground_epoch: 1,
                lease_id: Some(1),
            },
            state: AppInstanceState::Foreground,
            pid: None,
            title: String::new(),
            subtitle: String::new(),
            last_error: None,
        },
    );
    state.apps.insert(
        b.clone(),
        AppInstance {
            token: AppToken {
                app_id: b,
                generation: 1,
                foreground_epoch: 1,
                lease_id: Some(2),
            },
            state: AppInstanceState::Foreground,
            pid: None,
            title: String::new(),
            subtitle: String::new(),
            last_error: None,
        },
    );
    state.foreground_app = Some(a);
    assert_eq!(
        state.validate(),
        Err(StateModelError::MultipleForegroundApps)
    );
}

#[test]
fn v2_state_json_round_trip_preserves_fences() {
    let app = id("magicpaper");
    let mut state = managed_state();
    state.start_app(app.clone(), 99, Some(123)).unwrap();
    state.grant_foreground(&app, 99, 17, 8001).unwrap();
    let json = serde_json::to_vec(&state).unwrap();
    let decoded: SupervisorState = serde_json::from_slice(&json).unwrap();
    assert_eq!(decoded, state);
    assert!(decoded.validate().is_ok());
}
