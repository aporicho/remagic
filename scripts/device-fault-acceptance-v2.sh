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

assert_manager_stable() {
    local attempts
    attempts=0
    while [ "$attempts" -lt 20 ]; do
        "$CTL" status 2>/dev/null | grep -q '"domain": "manager"' \
            || fail "manager rollback was only transient"
        [ "$(systemctl is-active remagic-display-host.service 2>/dev/null || true)" = active ] \
            || fail "display host stopped after manager rollback"
        [ "$(systemctl is-active remagic-home.service 2>/dev/null || true)" = active ] \
            || fail "Home stopped after manager rollback"
        [ "$(systemctl is-active xochitl.service 2>/dev/null || true)" != active ] \
            || fail "stock shell reclaimed the display after manager rollback"
        sleep 0.1
        attempts=$((attempts + 1))
    done
}

display_number() {
    local field
    [ "$#" -eq 1 ] || return 1
    field=$1
    "$CTL" display-status 2>/dev/null \
        | sed -n "s/.*\"$field\": \([0-9][0-9]*\).*/\1/p" | sed -n '1p'
}

wait_panel_settled() {
    local attempts stable
    attempts=0 stable=0
    while [ "$attempts" -lt 160 ]; do
        if [ "$(display_number queue_depth)" = 0 ]; then
            stable=$((stable + 1))
            [ "$stable" -ge 4 ] && return 0
        else
            stable=0
        fi
        sleep 0.05
        attempts=$((attempts + 1))
    done
    fail "panel command queue did not settle"
}

full_refresh_checkpoint() {
    wait_panel_settled
    display_number full_refresh_count
}

assert_one_full_refresh_since() {
    local before label after
    [ "$#" -eq 2 ] || return 1
    before=$1 label=$2
    wait_panel_settled
    after=$(display_number full_refresh_count)
    [ "$after" -eq $((before + 1)) ] \
        || fail "$label changed full-refresh count from $before to $after"
}

main_pid() {
    systemctl show --property=MainPID --value "$1"
}

child_pid() {
    local unit main group pid attempts controller
    [ "$#" -eq 1 ] || return 1
    unit=$1
    attempts=0
    while [ "$attempts" -lt 100 ]; do
        main=$(main_pid "$unit")
        group=$(systemctl show --property=ControlGroup --value "$unit")
        for controller in /sys/fs/cgroup /sys/fs/cgroup/unified \
                /sys/fs/cgroup/systemd /sys/fs/cgroup/pids; do
            [ -r "$controller$group/cgroup.procs" ] || continue
            for pid in $(cat "$controller$group/cgroup.procs"); do
                [ "$pid" != "$main" ] && { echo "$pid"; return 0; }
            done
        done
        sleep 0.1
        attempts=$((attempts + 1))
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

echo "[v2-fault] failed cold launch rolls back with exactly one full refresh"
fault_manifest=$REMAGIC_TEST_ROOT/manifests/magicpaper.toml
saved_manifest=$REMAGIC_TEST_ROOT/magicpaper.toml.before-launch-fault
fault_root=$REMAGIC_TEST_ROOT/fail-launch
mkdir -p "$fault_root"
printf '%s\n' '#!/bin/sh' 'exit 17' >"$fault_root/fail-launch"
chmod 0700 "$fault_root/fail-launch"
cp "$fault_manifest" "$saved_manifest"
sed -e "s|^exec = .*|exec = \"$fault_root/fail-launch\"|" \
    -e "s|^working_dir = .*|working_dir = \"$fault_root\"|" \
    "$saved_manifest" >"$fault_manifest.tmp"
mv "$fault_manifest.tmp" "$fault_manifest"
"$CTL" reload >/dev/null
before_full=$(full_refresh_checkpoint)
if "$CTL" launch magicpaper >/dev/null 2>&1; then
    fail "failing MagicPaper executable was acknowledged as a successful launch"
fi
wait_domain '"domain": "manager"'
wait_not_active 'remagic-app@magicpaper.service'
assert_one_full_refresh_since "$before_full" "cold-launch rollback"
assert_manager_stable
mv "$saved_manifest" "$fault_manifest"
"$CTL" reload >/dev/null

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
attempts=0
while [ "$attempts" -lt 160 ]; do
    foreground_key=$("$CTL" display-status 2>/dev/null \
        | sed -n 's/.*"foreground_key": \([0-9][0-9]*\).*/\1/p')
    [ "$foreground_key" = 245209900 ] && break
    sleep 0.1
    attempts=$((attempts + 1))
done
[ "$foreground_key" = 245209900 ] \
    || fail "restarted Home was not rebound to the panel"

echo "[v2-fault] application child crash returns to Home"
"$CTL" launch magicpaper >/dev/null
wait_domain '"foreground": "magicpaper"'
magic_child=$(child_pid 'remagic-app@magicpaper.service') \
    || fail "MagicPaper child PID was not found"

echo "[v2-fault] failed park restores the same foreground with exactly one full refresh"
before_full=$(full_refresh_checkpoint)
kill -STOP "$magic_child"
( sleep 4; kill -CONT "$magic_child" 2>/dev/null || true ) &
resume_helper=$!
if "$CTL" park >/dev/null 2>&1; then
    kill -CONT "$magic_child" 2>/dev/null || true
    wait "$resume_helper" || true
    fail "stopped MagicPaper unexpectedly completed its park handshake"
fi
wait "$resume_helper" || true
wait_domain '"foreground": "magicpaper"'
[ "$(child_pid 'remagic-app@magicpaper.service')" = "$magic_child" ] \
    || fail "park recovery replaced the MagicPaper process"
assert_one_full_refresh_since "$before_full" "failed-park foreground recovery"

before_full=$(full_refresh_checkpoint)
kill -KILL "$magic_child"
wait_domain '"domain": "manager"'
wait_not_active 'remagic-app@magicpaper.service'
assert_one_full_refresh_since "$before_full" "MagicPaper crash recovery"

echo "[v2-fault] total runner loss is detected without its exit callback"
"$CTL" launch magicpaper >/dev/null
wait_domain '"foreground": "magicpaper"'
before_full=$(full_refresh_checkpoint)
systemctl kill --kill-who=all --signal=KILL 'remagic-app@magicpaper.service'
wait_not_active 'remagic-app@magicpaper.service'
wait_domain '"domain": "manager"'
assert_one_full_refresh_since "$before_full" "runner-loss recovery"

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
before_full=$(full_refresh_checkpoint)
kill -KILL "$koreader_child"
wait_domain '"domain": "manager"'
wait_not_active 'remagic-app@koreader.service'
assert_one_full_refresh_since "$before_full" "KOReader crash recovery"
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
