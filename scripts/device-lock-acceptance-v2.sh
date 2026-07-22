#!/bin/sh
set -eu

CTL=${REMAGIC_CTL:-/home/root/apps/remagic/bin/remagicctl}
HOME_KEY=245209900
UNLOCK_X=477
SUSPEND_SUCCESS=/sys/power/suspend_stats/success

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

read_suspend_success() {
    [ -r "$SUSPEND_SUCCESS" ] || fail "kernel suspend success counter is unavailable"
    value=$(cat "$SUSPEND_SUCCESS")
    case $value in
        ''|*[!0-9]*) fail "invalid kernel suspend success counter: ${value:-empty}" ;;
    esac
    printf '%s\n' "$value"
}

[ -x "$CTL" ] || fail "remagicctl is not installed"

echo "[lock-acceptance] enter manager"
"$CTL" manager >/dev/null
wait_domain manager
[ "$(display_number foreground_key)" = "$HOME_KEY" ] \
    || fail "Home is not the visible manager surface"
height=$(display_number physical_height)
[ -n "$height" ] && [ "$height" -gt 1200 ] || fail "invalid panel height: ${height:-missing}"
sleep_y=$((height - 98))
baseline=$(last_submission_sequence)
baseline_failures=$(display_number panel_failure_count)
[ -n "$baseline_failures" ] || fail "display status omitted panel failure count"
baseline_suspend_success=$(read_suspend_success)

echo "[lock-acceptance] the screen will suspend; press the physical power key once to wake it"
"$CTL" tap "$UNLOCK_X" "$sleep_y" >/dev/null
wait_domain sleeping
wait_submission_reason "$baseline" lock_screen 300

# Sleep returns only after a real kernel resume. Home must then prepare a
# replacement frame and enter Manager without any second touch interaction.
wait_domain manager 6000
wait_submission_reason "$baseline" unlock_screen 6000
final_suspend_success=$(read_suspend_success)
[ "$final_suspend_success" -gt "$baseline_suspend_success" ] \
    || fail "kernel never completed a real suspend/resume cycle"
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
