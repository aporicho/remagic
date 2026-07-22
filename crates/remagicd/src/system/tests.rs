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
