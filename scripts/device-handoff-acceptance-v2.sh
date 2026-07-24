#!/bin/sh
set -eu

CTL=${REMAGIC_CTL:-/home/root/apps/remagic/bin/remagicctl}
STARTED_AT=$(date +%s)
ISOLATION=${REMAGIC_TEST_ISOLATION:-/home/root/apps/remagic/libexec/device-test-isolation.sh}
REMAGIC_TEST_CTL=${REMAGIC_TEST_CTL:-$CTL}
[ -r "$ISOLATION" ] || { echo "[v2-handoff] missing isolation helper: $ISOLATION" >&2; exit 1; }
# shellcheck source=scripts/lib/device-test-isolation.sh
. "$ISOLATION"

fail() {
    echo "[v2-handoff] ERROR: $*" >&2
    exit 1
}

diagnostics() {
    "$CTL" status >&2 || true
    "$CTL" display-status >&2 || true
    "$CTL" display-submissions >&2 || true
    journalctl --since="@$STARTED_AT" --no-pager \
        -u remagicd.service -u remagic-display-host.service \
        -u 'remagic-app@magicpaper.service' -u 'remagic-app@koreader.service' \
        -o short-monotonic | tail -n 500 >&2 || true
}

cleanup() {
    cleanup_status=$1
    trap - EXIT HUP INT TERM
    [ "$cleanup_status" -eq 0 ] || diagnostics
    "$CTL" system >/dev/null 2>&1 \
        || /home/root/apps/remagic/libexec/remagic-recover >/dev/null 2>&1 \
        || true
    remagic_test_finish || cleanup_status=1
    exit "$cleanup_status"
}
trap 'cleanup "$?"' EXIT
trap 'exit 1' HUP INT TERM

wait_domain() {
    pattern=$1 attempts=${2:-300}
    while [ "$attempts" -gt 0 ]; do
        "$CTL" status 2>/dev/null | grep -q "$pattern" && return 0
        attempts=$((attempts - 1))
        sleep 0.1
    done
    fail "manager domain did not match $pattern"
}

main_pid() {
    systemctl show --property=MainPID --value "$1"
}

display_number() {
    field=$1
    "$CTL" display-status 2>/dev/null \
        | sed -n "s/.*\"$field\": \([0-9][0-9]*\).*/\1/p" | sed -n '1p'
}

wait_panel_settled() {
    attempts=0 stable=0
    while [ "$attempts" -lt 160 ]; do
        if [ "$(display_number queue_depth)" = 0 ]; then
            stable=$((stable + 1))
            [ "$stable" -ge 4 ] && return 0
        else
            stable=0
        fi
        attempts=$((attempts + 1))
        sleep 0.05
    done
    fail "panel command queue did not settle"
}

assert_freezer_state() {
    unit=$1 expected=$2
    actual=$(systemctl show --property=FreezerState --value "$unit" 2>/dev/null || true)
    [ "$actual" = "$expected" ] && return 0
    pid=$(main_pid "$unit")
    process_state=$(sed -n 's/^State:[[:space:]]*\([^[:space:]]\).*/\1/p' \
        "/proc/$pid/status" 2>/dev/null || true)
    case "$expected:$actual:$process_state" in
        frozen:running:T|frozen:running:t) return 0 ;;
        running:running:T|running:running:t) ;;
        running:running:?) return 0 ;;
    esac
    fail "$unit freezer/process state is $actual/$process_state instead of $expected"
}

wait_magicpaper_log() {
    pattern=$1 attempts=0
    while [ "$attempts" -lt 100 ]; do
        journalctl --since="@$STARTED_AT" --no-pager \
            -u 'remagic-app@magicpaper.service' 2>/dev/null \
            | grep -Eq "$pattern" && return 0
        attempts=$((attempts + 1))
        sleep 0.05
    done
    fail "MagicPaper log did not match: $pattern"
}

remagic_test_begin handoff || fail "could not establish isolated application data"

echo "[v2-handoff] prepare resident KOReader"
"$CTL" manager >/dev/null
wait_domain '"domain": "manager"'
"$CTL" launch koreader >/dev/null
wait_domain '"foreground": "koreader"'
koreader_pid=$(main_pid 'remagic-app@koreader.service')
[ "$koreader_pid" -gt 1 ] || fail "KOReader PID is missing"
"$CTL" park >/dev/null
wait_domain '"domain": "manager"'
assert_freezer_state 'remagic-app@koreader.service' frozen

echo "[v2-handoff] launch MagicPaper and inject one handwritten read command"
"$CTL" launch magicpaper >/dev/null
wait_domain '"foreground": "magicpaper"'
magicpaper_pid=$(main_pid 'remagic-app@magicpaper.service')
[ "$magicpaper_pid" -gt 1 ] || fail "MagicPaper PID is missing"
wait_panel_settled
before_full=$(display_number full_refresh_count)
test_event=$REMAGIC_TEST_ROOT/magicpaper/data/test-event
printf 'read\n' >"$test_event"
chmod 0600 "$test_event"
handoff_started=$(date +%s)
"$CTL" pen-line 220 620 690 720 >/dev/null
wait_domain '"foreground": "koreader"' 50
handoff_elapsed=$(($(date +%s) - handoff_started))
[ "$handoff_elapsed" -le 5 ] \
    || fail "read handoff took ${handoff_elapsed}s instead of at most 5s"

[ "$(main_pid 'remagic-app@koreader.service')" = "$koreader_pid" ] \
    || fail "read handoff did not recall the same KOReader process"
[ "$(main_pid 'remagic-app@magicpaper.service')" = "$magicpaper_pid" ] \
    || fail "read handoff replaced the MagicPaper process"
assert_freezer_state 'remagic-app@magicpaper.service' frozen
assert_freezer_state 'remagic-app@koreader.service' running
[ ! -e "$test_event" ] || fail "MagicPaper did not consume the read marker"
wait_panel_settled
after_full=$(display_number full_refresh_count)
[ "$after_full" -eq $((before_full + 1)) ] \
    || fail "read handoff changed full refreshes from $before_full to $after_full"
wait_magicpaper_log 'event=test-event-consumed command=read'
wait_magicpaper_log 'event=reader-handoff-accepted request=[^ ]+ app=koreader'

echo "[v2-handoff] PASS"
