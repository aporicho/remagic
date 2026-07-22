#!/bin/sh
set -eu

CTL=${REMAGIC_CTL:-/home/root/apps/remagic/bin/remagicctl}
STARTED_AT=$(date +%s)
ISOLATION=${REMAGIC_TEST_ISOLATION:-/home/root/apps/remagic/libexec/device-test-isolation.sh}
REMAGIC_TEST_CTL=${REMAGIC_TEST_CTL:-$CTL}
[ -r "$ISOLATION" ] || { echo "[v2-fault] missing isolation helper: $ISOLATION" >&2; exit 1; }
# shellcheck source=scripts/lib/device-test-isolation.sh
. "$ISOLATION"

fail() {
    echo "[v2-fault] ERROR: $*" >&2
    exit 1
}

wait_unit() {
    local unit wanted attempts
    [ "$#" -eq 2 ] || return 1
    unit=$1 wanted=$2 attempts=0
    while [ "$attempts" -lt 240 ]; do
        [ "$(systemctl is-active "$unit" 2>/dev/null || true)" = "$wanted" ] && return 0
        sleep 0.1
        attempts=$((attempts + 1))
    done
    fail "$unit did not become $wanted"
}

wait_not_active() {
    local unit attempts actual
    [ "$#" -eq 1 ] || return 1
    unit=$1 attempts=0
    while [ "$attempts" -lt 240 ]; do
        actual=$(systemctl is-active "$unit" 2>/dev/null || true)
        [ "$actual" != active ] && [ "$actual" != activating ] && return 0
        sleep 0.1
        attempts=$((attempts + 1))
    done
    fail "$unit remained active"
}

wait_domain() {
    local pattern attempts
    [ "$#" -eq 1 ] || return 1
    pattern=$1 attempts=0
    while [ "$attempts" -lt 300 ]; do
        "$CTL" status 2>/dev/null | grep -q "$pattern" && return 0
        sleep 0.1
        attempts=$((attempts + 1))
    done
    fail "manager domain did not match $pattern"
}

main_pid() {
    systemctl show --property=MainPID --value "$1"
}

child_pid() {
    local unit main group pid
    [ "$#" -eq 1 ] || return 1
    unit=$1
    main=$(main_pid "$unit")
    group=$(systemctl show --property=ControlGroup --value "$unit")
    for pid in $(cat "/sys/fs/cgroup$group/cgroup.procs" 2>/dev/null || true); do
        [ "$pid" != "$main" ] && { echo "$pid"; return 0; }
    done
    return 1
}

lifecycle_generation() {
    local app
    [ "$#" -eq 1 ] || return 1
    app=$1
    sed -n 's/.*"generation":[[:space:]]*\([0-9][0-9]*\).*/\1/p' \
        "/run/remagic/apps/$app/lifecycle-status.json" | sed -n '1p'
}

managed_wakelock_is_active() {
    [ -r /sys/power/wake_lock ] || return 2
    tr ' ' '\n' </sys/power/wake_lock | grep -Fxq remagic-managed
}

assert_managed_wakelock() {
    managed_wakelock_is_active || fail "managed domain has no active remagic-managed wakelock"
}

assert_no_managed_wakelock() {
    managed_wakelock_is_active && fail "stock domain retained remagic-managed wakelock"
    status=$?
    [ "$status" -eq 1 ] || fail "cannot inspect /sys/power/wake_lock"
}

diagnostics() {
    "$CTL" status >&2 || true
    "$CTL" display-status >&2 || true
    systemctl status --no-pager remagicd.service remagic-display-host.service \
        remagic-home.service 'remagic-app@magicpaper.service' \
        'remagic-app@koreader.service' xochitl.service >&2 || true
    journalctl --since="@$STARTED_AT" --no-pager \
        -u remagicd.service -u remagic-display-host.service -u remagic-home.service \
        -u 'remagic-app@magicpaper.service' -u 'remagic-app@koreader.service' \
        -o short-monotonic | tail -n 600 >&2 || true
}

cleanup() {
    local cleanup_status
    [ "$#" -eq 1 ] || exit 1
    cleanup_status=$1
    trap - EXIT HUP INT TERM
    if [ "$cleanup_status" -ne 0 ]; then diagnostics; fi
    "$CTL" system >/dev/null 2>&1 \
        || /home/root/apps/remagic/libexec/remagic-recover >/dev/null 2>&1 \
        || true
    remagic_test_finish || cleanup_status=1
    exit "$cleanup_status"
}
trap 'cleanup "$?"' EXIT
trap 'exit 1' HUP INT TERM

remagic_test_begin fault || fail "could not establish isolated application data"

echo "[v2-fault] establish managed Home"
wait_unit xochitl.service active
"$CTL" manager >/dev/null
wait_domain '"domain": "manager"'
assert_managed_wakelock

echo "[v2-fault] Home process crash is restarted and rebound"
old_home=$(main_pid remagic-home.service)
[ "$old_home" -gt 1 ] || fail "Home has no live PID"
kill -KILL "$old_home"
attempts=0
while [ "$attempts" -lt 160 ]; do
    new_home=$(main_pid remagic-home.service)
    [ "$new_home" -gt 1 ] && [ "$new_home" != "$old_home" ] && break
    sleep 0.1
    attempts=$((attempts + 1))
done
[ "$new_home" != "$old_home" ] || fail "Home did not restart after SIGKILL"
wait_domain '"domain": "manager"'
[ "$("$CTL" display-status | sed -n 's/.*"foreground_key": \([0-9][0-9]*\).*/\1/p')" = 245209900 ] \
    || fail "restarted Home was not rebound to the panel"

echo "[v2-fault] application child crash returns to Home"
"$CTL" launch magicpaper >/dev/null
wait_domain '"foreground": "magicpaper"'
magic_child=$(child_pid 'remagic-app@magicpaper.service') \
    || fail "MagicPaper child PID was not found"
kill -KILL "$magic_child"
wait_domain '"domain": "manager"'
wait_not_active 'remagic-app@magicpaper.service'

echo "[v2-fault] total runner loss is detected without its exit callback"
"$CTL" launch magicpaper >/dev/null
wait_domain '"foreground": "magicpaper"'
systemctl kill --kill-who=all --signal=KILL 'remagic-app@magicpaper.service'
wait_not_active 'remagic-app@magicpaper.service'
wait_domain '"domain": "manager"'

echo "[v2-fault] stale exit token cannot evict a live replacement"
"$CTL" launch koreader >/dev/null
wait_domain '"foreground": "koreader"'
old_generation=$(lifecycle_generation koreader)
remagic_test_u64_nonzero "$old_generation" || fail "KOReader generation is missing"
"$CTL" park >/dev/null
wait_domain '"domain": "manager"'
"$CTL" close koreader --complete >/dev/null
wait_not_active 'remagic-app@koreader.service'
"$CTL" launch koreader >/dev/null
wait_domain '"foreground": "koreader"'
koreader_pid=$(main_pid 'remagic-app@koreader.service')
live_generation=$(lifecycle_generation koreader)
remagic_test_u64_greater "$live_generation" "$old_generation" \
    || fail "replacement KOReader did not receive a newer generation"
"$CTL" runtime-exited koreader --generation "$old_generation" --exit-code 1 --crashed >/dev/null
wait_domain '"foreground": "koreader"'
[ "$(main_pid 'remagic-app@koreader.service')" = "$koreader_pid" ] \
    || fail "stale exit token terminated live KOReader"

echo "[v2-fault] KOReader process crash is supervised independently"
koreader_child=$(child_pid 'remagic-app@koreader.service') \
    || fail "KOReader child PID was not found"
kill -KILL "$koreader_child"
wait_domain '"domain": "manager"'
wait_not_active 'remagic-app@koreader.service'
"$CTL" launch koreader >/dev/null
wait_domain '"foreground": "koreader"'

echo "[v2-fault] display-host crash restores the stock owner"
systemctl kill --kill-who=all --signal=KILL remagic-display-host.service
wait_not_active remagic-display-host.service
wait_unit xochitl.service active
wait_domain '"domain": "system"'
assert_no_managed_wakelock

echo "[v2-fault] daemon crash also converges to stock"
"$CTL" manager >/dev/null
wait_domain '"domain": "manager"'
systemctl kill --kill-who=all --signal=KILL remagicd.service
wait_unit remagicd.service active
wait_unit xochitl.service active
wait_not_active remagic-display-host.service
wait_domain '"domain": "system"'
assert_no_managed_wakelock

echo "[v2-fault] PASS"
