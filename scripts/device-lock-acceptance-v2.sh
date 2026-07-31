#!/bin/sh
set -eu

CTL=${REMAGIC_CTL:-/home/root/apps/remagic/bin/remagicctl}
HOME_KEY=245209900
SUSPEND_SUCCESS=/sys/power/suspend_stats/success
REAL_SUSPEND=${REMAGIC_REAL_SUSPEND:-0}

fail() {
    echo "[lock-acceptance] FAIL: $*" >&2
    "$CTL" status >&2 || true
    "$CTL" display-status >&2 || true
    "$CTL" display-submissions >&2 || true
    exit 1
}

status() {
    "$CTL" status
}

display_status() {
    "$CTL" display-status
}

display_number() {
    field=$1
    display_status | sed -n "s/.*\"$field\": \([0-9][0-9]*\).*/\1/p" | sed -n '1p'
}

submissions() {
    "$CTL" display-submissions
}

last_submission_sequence() {
    submissions | awk -F '\t' 'NR > 1 && $1 ~ /^[0-9]+$/ { value=$1 } END { print value+0 }'
}

wait_domain() {
    expected=$1
    attempts=${2:-300}
    while [ "$attempts" -gt 0 ]; do
        current_status=$(status)
        if printf '%s\n' "$current_status" | grep -Fq "\"domain\": \"$expected\""; then
            return 0
        fi
        attempts=$((attempts - 1))
        sleep 0.1
    done
    fail "domain did not become $expected"
}

wait_submission_reason() {
    baseline=$1
    reason=$2
    attempts=${3:-6000}
    while [ "$attempts" -gt 0 ]; do
        if submissions | awk -F '\t' -v baseline="$baseline" -v reason="$reason" \
            'NR > 1 && $1 > baseline && $7 == reason && $10 == "true" { found=1 } END { exit !found }'; then
            return 0
        fi
        attempts=$((attempts - 1))
        sleep 0.1
    done
    fail "no successful $reason panel submission appeared"
}

assert_no_submission_reason() {
    baseline=$1
    reason=$2
    if submissions | awk -F '\t' -v baseline="$baseline" -v reason="$reason" \
        'NR > 1 && $1 > baseline && $7 == reason && $10 == "true" { found=1 } END { exit !found }'; then
        fail "unexpected $reason panel submission appeared"
    fi
}

assert_full_refresh_delta() {
    before=$1
    delta=$2
    label=$3
    after=$(display_number full_refresh_count)
    expected=$((before + delta))
    [ "$after" -eq "$expected" ] \
        || fail "$label changed full-refresh count from $before to $after instead of $expected"
}

read_suspend_success() {
    [ -r "$SUSPEND_SUCCESS" ] || fail "kernel suspend success counter is unavailable"
    value=$(cat "$SUSPEND_SUCCESS")
    case $value in
        ''|*[!0-9]*) fail "invalid kernel suspend success counter: ${value:-empty}" ;;
    esac
    printf '%s\n' "$value"
}

wait_external_wake_locks_clear() {
    attempts=${1:-1200}
    while [ "$attempts" -gt 0 ]; do
        active_locks=$(cat /sys/power/wake_lock 2>/dev/null || true)
        blockers=$(printf '%s\n' "$active_locks" | tr ' ' '\n' \
            | sed '/^$/d;/^remagic-managed$/d')
        [ -z "$blockers" ] && return 0
        attempts=$((attempts - 1))
        sleep 0.5
    done
    fail "external wake locks did not clear after unplug: ${blockers:-unknown}"
}

[ -x "$CTL" ] || fail "remagicctl is not installed"

echo "[lock-acceptance] enter manager"
"$CTL" manager >/dev/null
wait_domain manager
[ "$(display_number foreground_key)" = "$HOME_KEY" ] \
    || fail "Home is not the visible manager surface"
height=$(display_number physical_height)
[ -n "$height" ] && [ "$height" -gt 1200 ] || fail "invalid panel height: ${height:-missing}"
width=$(display_number physical_width)
[ -n "$width" ] && [ "$width" -gt 800 ] || fail "invalid panel width: ${width:-missing}"
button_w=142
button_h=72
gap=18
settings_x=$((width - 48 - button_w))
sleep_x=$((settings_x - gap - button_w + button_w / 2))
sleep_y=$((38 + button_h / 2))
baseline=$(last_submission_sequence)
baseline_full=$(display_number full_refresh_count)
baseline_failures=$(display_number panel_failure_count)
[ -n "$baseline_failures" ] || fail "display status omitted panel failure count"
baseline_suspend_success=$(read_suspend_success)

echo "[lock-acceptance] verify charger-blocked lock and single-power unlock"
wake_locks=$(cat /sys/power/wake_lock 2>/dev/null || true)
case " $wake_locks " in
    *" remagic-managed "*) ;;
    *) fail "managed wake lock is not held" ;;
esac
external_wake_locks=$(printf '%s\n' "$wake_locks" | tr ' ' '\n' \
    | sed '/^$/d;/^remagic-managed$/d')
[ -n "$external_wake_locks" ] \
    || fail "plugged acceptance requires an external charger wake lock"
"$CTL" tap "$sleep_x" "$sleep_y" >/dev/null 2>&1 || true
wait_domain sleeping
wait_submission_reason "$baseline" lock_screen 300
assert_full_refresh_delta "$baseline_full" 1 "lock presentation"

# `park` is the control-plane equivalent of one power click. In Sleeping it
# must request Home's normal resume_unlock flow instead of re-entering suspend.
"$CTL" park >/dev/null
wait_domain manager 600
wait_submission_reason "$baseline" unlock_screen 600
[ "$(read_suspend_success)" -eq "$baseline_suspend_success" ] \
    || fail "charger-blocked scenario unexpectedly completed kernel suspend"
assert_full_refresh_delta "$baseline_full" 2 "plugged lock/unlock"
assert_no_submission_reason "$baseline" lock_refresh
[ "$(display_number lock_epoch)" = 0 ] || fail "lock epoch survived plugged unlock"

[ "$REAL_SUSPEND" = 1 ] || {
    [ "$(display_number panel_failure_count)" = "$baseline_failures" ] \
        || fail "panel failure count changed during plugged lock transaction"
    echo "[lock-acceptance] PASS (plugged path; set REMAGIC_REAL_SUSPEND=1 for unplugged suspend)"
    exit 0
}

echo "[lock-acceptance] unplug power; then press the physical power key once after suspend"
wait_external_wake_locks_clear
baseline=$(last_submission_sequence)
baseline_full=$(display_number full_refresh_count)
baseline_suspend_success=$(read_suspend_success)
"$CTL" tap "$sleep_x" "$sleep_y" >/dev/null
wait_domain sleeping
wait_submission_reason "$baseline" lock_screen 300

# Sleep returns only after a real kernel resume. Home must then prepare a
# replacement frame and enter Manager without any second touch interaction.
wait_domain manager 6000
wait_submission_reason "$baseline" unlock_screen 6000
final_suspend_success=$(read_suspend_success)
[ "$final_suspend_success" -gt "$baseline_suspend_success" ] \
    || fail "kernel never completed a real suspend/resume cycle"
assert_full_refresh_delta "$baseline_full" 2 "physical lock/resume"
assert_no_submission_reason "$baseline" lock_refresh
[ "$(display_number lock_epoch)" = 0 ] || fail "lock epoch survived unlock"
current_display_status=$(display_status)
printf '%s\n' "$current_display_status" | grep -Fq '"lock_committed": false' \
    || fail "panel still reports a committed lock"
[ "$(display_number foreground_key)" = "$HOME_KEY" ] \
    || fail "power-key wake did not return to Home"
[ "$(display_number panel_failure_count)" = "$baseline_failures" ] \
    || fail "panel failure count changed during the lock transaction"

echo "[lock-acceptance] PASS"
