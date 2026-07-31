use super::*;

#[test]
fn wake_unlock_is_idempotent_when_the_kernel_reports_absent_name() {
    assert_eq!(
        interpret_wake_unlock(Err(io::Error::from_raw_os_error(libc::EINVAL))),
        Ok(())
    );
}

#[test]
fn wake_unlock_keeps_real_kernel_failures_visible() {
    let error = interpret_wake_unlock(Err(io::Error::from_raw_os_error(13))).unwrap_err();
    assert!(error.contains("cannot release managed wake lock"));
}

#[test]
fn suspend_success_counter_is_strictly_numeric() {
    assert_eq!(parse_suspend_success("42\n"), Ok(42));
    assert!(parse_suspend_success("").is_err());
    assert!(parse_suspend_success("41 wakeups").is_err());
}

#[test]
fn suspend_preflight_ignores_our_lock_but_reports_external_owners() {
    assert!(external_wake_locks("remagic-managed\n").is_empty());
    assert_eq!(
        external_wake_locks("remagic-managed udev.charger wifi"),
        vec!["udev.charger", "wifi"]
    );
}

#[test]
fn active_wakeup_summary_ignores_the_managed_wakelock() {
    let snapshot = WakeupSnapshot {
        available: true,
        sources: vec![
            WakeupSource {
                name: "remagic-managed".into(),
                active_time_ms: 200,
                ..WakeupSource::default()
            },
            WakeupSource {
                name: "1-0048".into(),
                active_time_ms: 42,
                ..WakeupSource::default()
            },
        ],
    };

    assert_eq!(
        active_wakeup_source_summary(&snapshot),
        "1-0048(active_ms=42)"
    );
}

#[test]
fn wakeup_delta_summary_reports_top_changes() {
    let before = WakeupSnapshot {
        available: true,
        sources: vec![WakeupSource {
            name: "1-0048".into(),
            active_count: 10,
            event_count: 20,
            wakeup_count: 1,
            prevent_suspend_time_ms: 100,
            active_time_ms: 0,
        }],
    };
    let after = WakeupSnapshot {
        available: true,
        sources: vec![
            WakeupSource {
                name: "remagic-managed".into(),
                active_count: 99,
                event_count: 99,
                wakeup_count: 99,
                prevent_suspend_time_ms: 99,
                active_time_ms: 99,
            },
            WakeupSource {
                name: "1-0048".into(),
                active_count: 12,
                event_count: 25,
                wakeup_count: 2,
                prevent_suspend_time_ms: 145,
                active_time_ms: 0,
            },
        ],
    };

    assert_eq!(
        wakeup_source_delta_summary(&before, &after),
        "1-0048(active_ms=0, active+2, events+5, wakeups+1, prevent_ms+45)"
    );
}
