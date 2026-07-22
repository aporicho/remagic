#!/bin/sh
set -eu

CTL=${REMAGIC_CTL:-/home/root/apps/remagic/bin/remagicctl}
CYCLES=${REMAGIC_STRESS_CYCLES:-10}
MAX_WARM_SWITCH_MS=${REMAGIC_MAX_WARM_SWITCH_MS:-5000}
MAX_HOST_RSS_GROWTH_KB=${REMAGIC_MAX_HOST_RSS_GROWTH_KB:-16384}
MAX_APP_RSS_GROWTH_KB=${REMAGIC_MAX_APP_RSS_GROWTH_KB:-32768}
MAX_FD_GROWTH=${REMAGIC_MAX_FD_GROWTH:-8}
ISOLATION=${REMAGIC_TEST_ISOLATION:-/home/root/apps/remagic/libexec/device-test-isolation.sh}
REMAGIC_TEST_CTL=${REMAGIC_TEST_CTL:-$CTL}
[ -r "$ISOLATION" ] || { echo "[v2-stress] missing isolation helper: $ISOLATION" >&2; exit 1; }
# shellcheck source=scripts/lib/device-test-isolation.sh
. "$ISOLATION"

CYCLES=$(remagic_test_u64_canonical "$CYCLES") || {
    echo "[v2-stress] CYCLES must be a positive decimal integer" >&2
    exit 2
}
remagic_test_u64_nonzero "$CYCLES" && ! remagic_test_u64_greater "$CYCLES" 10000 || {
    echo "[v2-stress] CYCLES must be between 1 and 10000" >&2
    exit 2
}
MAX_WARM_SWITCH_MS=$(remagic_test_u64_canonical "$MAX_WARM_SWITCH_MS") || {
    echo "[v2-stress] MAX_WARM_SWITCH_MS must be a positive decimal integer" >&2
    exit 2
}
remagic_test_u64_nonzero "$MAX_WARM_SWITCH_MS" &&
    ! remagic_test_u64_greater "$MAX_WARM_SWITCH_MS" 600000 || {
        echo "[v2-stress] MAX_WARM_SWITCH_MS must be between 1 and 600000" >&2
        exit 2
    }
MAX_HOST_RSS_GROWTH_KB=$(remagic_test_u64_canonical "$MAX_HOST_RSS_GROWTH_KB") || exit 2
MAX_APP_RSS_GROWTH_KB=$(remagic_test_u64_canonical "$MAX_APP_RSS_GROWTH_KB") || exit 2
MAX_FD_GROWTH=$(remagic_test_u64_canonical "$MAX_FD_GROWTH") || exit 2
for limit in "$MAX_HOST_RSS_GROWTH_KB" "$MAX_APP_RSS_GROWTH_KB" "$MAX_FD_GROWTH"; do
    ! remagic_test_u64_greater "$limit" 2147483647 || {
        echo "[v2-stress] resource limits must fit a signed 32-bit counter" >&2
        exit 2
    }
done

fail() {
    echo "[v2-stress] ERROR: $*" >&2
    exit 1
}

now_ms() {
    # awk numbers are doubles; %.0f avoids the signed-32-bit `%d` overflow
    # that appears after roughly 24.8 days of device uptime.
    awk '{ printf "%.0f\n", $1 * 1000 }' /proc/uptime
}

display_number() {
    local field
    [ "$#" -eq 1 ] || return 1
    field=$1
    "$CTL" display-status | sed -n "s/.*\"$field\": \([0-9][0-9]*\).*/\1/p" | sed -n '1p'
}

display_submissions() {
    "$CTL" display-submissions
}

last_submission_sequence() {
    local submissions
    submissions=$(display_submissions) || fail "could not read panel submission evidence"
    printf '%s\n' "$submissions" | awk -F '\t' '
        NR == 1 {
            if (NF != 10 || $1 != "sequence" || $2 != "surface_sequence" ||
                $3 != "key" || $4 != "generation" || $5 != "foreground_epoch" ||
                $6 != "intent" || $7 != "reason" || $8 != "visible_signature" ||
                $9 != "marker" || $10 != "success") exit 2
            next
        }
        { last = $1 }
        END { if (last == "") print 0; else print last }
    '
}

assert_exact_foreground_submission_since() {
    local baseline key generation epoch label submissions count valid
    [ "$#" -eq 5 ] || return 1
    baseline=$1 key=$2 generation=$3 epoch=$4 label=$5
    wait_queue_empty
    submissions=$(display_submissions)
    count=$(printf '%s\n' "$submissions" | awk -F '\t' \
        -v baseline="$baseline" -v key="$key" -v generation="$generation" -v epoch="$epoch" '
            function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
            function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
            function ugt(a, b, ca, cb, la, lb) {
                ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                if (la != lb) return la > lb
                return ("u" ca) > ("u" cb)
            }
            NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                ueq($4, generation) && ueq($5, epoch) && $6 == "full" &&
                $7 == "foreground_switch" { count++ }
            END { print count + 0 }
        ')
    [ "$count" -eq 1 ] \
        || fail "$label has $count exact-lease foreground submissions instead of one"
    valid=$(printf '%s\n' "$submissions" | awk -F '\t' \
        -v baseline="$baseline" -v key="$key" -v generation="$generation" -v epoch="$epoch" '
            function canon(v) { sub(/^0+/, "", v); return v == "" ? "0" : v }
            function ueq(a, b) { return ("u" canon(a)) == ("u" canon(b)) }
            function ugt(a, b, ca, cb, la, lb) {
                ca=canon(a); cb=canon(b); la=length(ca); lb=length(cb)
                if (la != lb) return la > lb
                return ("u" ca) > ("u" cb)
            }
            function unz(v) { return v ~ /^[0-9]+$/ && v ~ /[1-9]/ }
            NR > 1 && ugt($1, baseline) && ueq($3, key) &&
                ueq($4, generation) && ueq($5, epoch) && $6 == "full" &&
                $7 == "foreground_switch" && unz($2) && unz($8) &&
                unz($9) && $10 == "true" { count++ }
            END { print count + 0 }
        ')
    [ "$valid" -eq 1 ] || fail "$label lacks successful exact-lease panel evidence"
}

wait_domain() {
    local pattern attempts
    [ "$#" -eq 1 ] || return 1
    pattern=$1 attempts=0
    while [ "$attempts" -lt 240 ]; do
        "$CTL" status 2>/dev/null | grep -q "$pattern" && return 0
        sleep 0.05
        attempts=$((attempts + 1))
    done
    fail "manager domain did not match $pattern"
}

wait_queue_empty() {
    local attempts
    attempts=0
    while [ "$attempts" -lt 200 ]; do
        [ "$(display_number queue_depth)" = 0 ] && return 0
        sleep 0.02
        attempts=$((attempts + 1))
    done
    fail "panel command queue did not drain"
}

main_pid() {
    systemctl show --property=MainPID --value "$1"
}

assert_freezer_state() {
    local unit expected actual pid process_state
    [ "$#" -eq 2 ] || return 1
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

unit_pids() {
    local unit group pid
    [ "$#" -eq 1 ] || return 1
    unit=$1
    group=$(systemctl show --property=ControlGroup --value "$unit" 2>/dev/null || true)
    if [ -n "$group" ] && [ -r "/sys/fs/cgroup$group/cgroup.procs" ]; then
        cat "/sys/fs/cgroup$group/cgroup.procs"
    else
        pid=$(main_pid "$unit")
        [ "${pid:-0}" -gt 1 ] && printf '%s\n' "$pid"
    fi
}

unit_rss_kb() {
    unit_pids "$1" | while IFS= read -r pid; do
        awk '/^VmRSS:/ { print $2; found=1 } END { if (!found) print 0 }' "/proc/$pid/status" 2>/dev/null || true
    done | awk '{ total += $1 } END { printf "%.0f\n", total }'
}

unit_fd_count() {
    unit_pids "$1" | while IFS= read -r pid; do
        set -- "/proc/$pid/fd/"*
        if [ "$1" = "/proc/$pid/fd/*" ]; then printf '0\n'; else printf '%s\n' "$#"; fi
    done | awk '{ total += $1 } END { print total + 0 }'
}

assert_single_refresh() {
    local before label after
    [ "$#" -eq 2 ] || return 1
    before=$1 label=$2
    wait_queue_empty
    sleep 0.2
    after=$(display_number full_refresh_count)
    remagic_test_u64_is_next "$before" "$after" \
        || fail "$label full-refresh count was $before then $after instead of one increment"
}

cleanup() {
    local cleanup_status
    [ "$#" -eq 1 ] || exit 1
    cleanup_status=$1
    trap - EXIT HUP INT TERM
    "$CTL" system >/dev/null 2>&1 \
        || /home/root/apps/remagic/libexec/remagic-recover >/dev/null 2>&1 \
        || true
    remagic_test_finish || cleanup_status=1
    exit "$cleanup_status"
}
trap 'cleanup "$?"' EXIT
trap 'exit 1' HUP INT TERM

remagic_test_begin stress || fail "could not establish isolated application data"

"$CTL" manager >/dev/null
wait_domain '"domain": "manager"'

echo "[v2-stress] cold-start both resident applications"
cold_sequence=$(last_submission_sequence)
"$CTL" launch magicpaper >/dev/null
wait_domain '"foreground": "magicpaper"'
magic_pid=$(main_pid 'remagic-app@magicpaper.service')
magic_key=$(display_number foreground_key)
assert_exact_foreground_submission_since "$cold_sequence" "$magic_key" \
    "$(display_number generation)" "$(display_number foreground_epoch)" "MagicPaper cold start"
"$CTL" park >/dev/null
wait_domain '"domain": "manager"'
cold_sequence=$(last_submission_sequence)
"$CTL" launch koreader >/dev/null
wait_domain '"foreground": "koreader"'
koreader_pid=$(main_pid 'remagic-app@koreader.service')
koreader_key=$(display_number foreground_key)
assert_exact_foreground_submission_since "$cold_sequence" "$koreader_key" \
    "$(display_number generation)" "$(display_number foreground_epoch)" "KOReader cold start"
"$CTL" park >/dev/null
wait_domain '"domain": "manager"'
assert_freezer_state 'remagic-app@koreader.service' frozen

[ "$magic_pid" -gt 1 ] && [ "$koreader_pid" -gt 1 ] || fail "resident PID is missing"
[ "$magic_key" != "$koreader_key" ] || fail "applications share one QTFB key"

# Warm the direct-ink and deterministic local-reply paths before measuring
# growth, so one-time font/layout allocation is not misclassified as a leak.
"$CTL" launch magicpaper >/dev/null
wait_domain '"foreground": "magicpaper"'
"$CTL" pen-line 180 620 420 680 >/dev/null
wait_queue_empty
sleep 0.5
"$CTL" park >/dev/null
wait_domain '"domain": "manager"'
wait_queue_empty
host_pid=$(main_pid remagic-display-host.service)
host_rss_before=$(unit_rss_kb remagic-display-host.service)
host_fds_before=$(unit_fd_count remagic-display-host.service)
magic_rss_before=$(unit_rss_kb remagic-app@magicpaper.service)
koreader_rss_before=$(unit_rss_kb remagic-app@koreader.service)
backpressure_before=$(display_number input_backpressure_events)

total_ms=0
max_ms=0
cycle=1
while [ "$cycle" -le "$CYCLES" ]; do
    wait_queue_empty
    before_full=$(display_number full_refresh_count)
    before_sequence=$(last_submission_sequence)
    start=$(now_ms)
    "$CTL" launch magicpaper >/dev/null
    wait_domain '"foreground": "magicpaper"'
    elapsed=$(( $(now_ms) - start ))
    total_ms=$((total_ms + elapsed))
    [ "$elapsed" -le "$MAX_WARM_SWITCH_MS" ] \
        || fail "MagicPaper warm recall took ${elapsed}ms"
    [ "$elapsed" -le "$max_ms" ] || max_ms=$elapsed
    [ "$(main_pid 'remagic-app@magicpaper.service')" = "$magic_pid" ] \
        || fail "MagicPaper restarted in cycle $cycle"
    [ "$(display_number foreground_key)" = "$magic_key" ] \
        || fail "MagicPaper surface changed in cycle $cycle"
    assert_single_refresh "$before_full" "MagicPaper cycle $cycle"
    assert_exact_foreground_submission_since "$before_sequence" "$magic_key" \
        "$(display_number generation)" "$(display_number foreground_epoch)" \
        "MagicPaper cycle $cycle"
    "$CTL" pen-line 180 620 420 680 >/dev/null
    wait_queue_empty
    before_full=$(display_number full_refresh_count)
    "$CTL" park >/dev/null
    wait_domain '"domain": "manager"'
    wait_queue_empty
    [ "$(display_number full_refresh_count)" = "$before_full" ] \
        || fail "MagicPaper park fully refreshed in cycle $cycle"

    wait_queue_empty
    before_full=$(display_number full_refresh_count)
    before_sequence=$(last_submission_sequence)
    start=$(now_ms)
    "$CTL" launch koreader >/dev/null
    wait_domain '"foreground": "koreader"'
    assert_freezer_state 'remagic-app@koreader.service' running
    elapsed=$(( $(now_ms) - start ))
    total_ms=$((total_ms + elapsed))
    [ "$elapsed" -le "$MAX_WARM_SWITCH_MS" ] \
        || fail "KOReader warm recall took ${elapsed}ms"
    [ "$elapsed" -le "$max_ms" ] || max_ms=$elapsed
    [ "$(main_pid 'remagic-app@koreader.service')" = "$koreader_pid" ] \
        || fail "KOReader restarted in cycle $cycle"
    [ "$(display_number foreground_key)" = "$koreader_key" ] \
        || fail "KOReader surface changed in cycle $cycle"
    assert_single_refresh "$before_full" "KOReader cycle $cycle"
    assert_exact_foreground_submission_since "$before_sequence" "$koreader_key" \
        "$(display_number generation)" "$(display_number foreground_epoch)" \
        "KOReader cycle $cycle"
    "$CTL" park >/dev/null
    wait_domain '"domain": "manager"'
    assert_freezer_state 'remagic-app@koreader.service' frozen
    wait_queue_empty
    [ "$(display_number panel_failure_count)" = 0 ] \
        || fail "panel failure recorded in cycle $cycle"
    cycle=$((cycle + 1))
done

switches=$((CYCLES * 2))
average_ms=$((total_ms / switches))
echo "[v2-stress] warm switches=$switches average=${average_ms}ms max=${max_ms}ms"

[ "$(main_pid remagic-display-host.service)" = "$host_pid" ] \
    || fail "display host restarted during stress run"
host_rss_after=$(unit_rss_kb remagic-display-host.service)
host_fds_after=$(unit_fd_count remagic-display-host.service)
magic_rss_after=$(unit_rss_kb remagic-app@magicpaper.service)
koreader_rss_after=$(unit_rss_kb remagic-app@koreader.service)
host_rss_growth=$((host_rss_after - host_rss_before))
app_rss_growth=$((magic_rss_after + koreader_rss_after - magic_rss_before - koreader_rss_before))
fd_growth=$((host_fds_after - host_fds_before))
[ "$host_rss_growth" -le "$MAX_HOST_RSS_GROWTH_KB" ] \
    || fail "display host RSS grew ${host_rss_growth}KiB"
[ "$app_rss_growth" -le "$MAX_APP_RSS_GROWTH_KB" ] \
    || fail "resident application RSS grew ${app_rss_growth}KiB"
[ "$fd_growth" -le "$MAX_FD_GROWTH" ] || fail "display host leaked $fd_growth file descriptors"
[ "$(display_number input_backpressure_events)" = "$backpressure_before" ] \
    || fail "input backpressure increased during ordinary stress strokes"
echo "[v2-stress] resource growth host_rss=${host_rss_growth}KiB apps_rss=${app_rss_growth}KiB host_fds=$fd_growth"

"$CTL" close koreader --complete >/dev/null
"$CTL" close magicpaper --complete >/dev/null
"$CTL" system >/dev/null
wait_domain '"domain": "system"'
echo "[v2-stress] PASS"
