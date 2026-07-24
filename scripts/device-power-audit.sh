#!/bin/sh
set -eu

STATE_ROOT=${REMAGIC_POWER_AUDIT_ROOT:-/home/root/.local/state/remagic/power-audit}
CURRENT=$STATE_ROOT/current
REMAGICCTL=/home/root/apps/remagic/bin/remagicctl
UNITS='remagicd.service remagic-display-host.service remagic-home.service remagic-agentd.service remagic-background-magicpaper.service magicpaper-agent.service remagic-app@magicpaper.service remagic-app@koreader.service'

fail() {
    echo "[power-audit] FAIL: $*" >&2
    exit 1
}

read_first() {
    for path in "$@"; do
        if [ -r "$path" ]; then
            sed -n '1p' "$path"
            return
        fi
    done
    printf 'unavailable\n'
}

battery_root() {
    for root in /sys/class/power_supply/*; do
        [ -r "$root/type" ] || continue
        [ "$(sed -n '1p' "$root/type")" = Battery ] || continue
        printf '%s\n' "$root"
        return
    done
}

snapshot() {
    destination=$1
    battery=$(battery_root || true)
    mkdir -p "$destination"
    date +%s >"$destination/unix"
    cat /proc/uptime >"$destination/uptime"
    read_first /sys/power/suspend_stats/success >"$destination/suspend-success"
    read_first /sys/power/suspend_stats/fail >"$destination/suspend-fail"
    read_first /sys/power/wake_lock >"$destination/wake-lock"
    if [ -n "$battery" ]; then
        for field in status capacity energy_now charge_now current_now voltage_now; do
            read_first "$battery/$field" >"$destination/battery-$field"
        done
    fi
    for unit in $UNITS; do
        safe=$(printf '%s' "$unit" | tr '@.' '__')
        systemctl show "$unit" \
            -p ActiveState -p SubState -p NRestarts -p CPUUsageNSec \
            >"$destination/unit-$safe" 2>/dev/null || true
    done
    if [ -x "$REMAGICCTL" ]; then
        "$REMAGICCTL" power >"$destination/power.json" 2>&1 || true
    fi
}

begin() {
    mkdir -p "$STATE_ROOT"
    temporary=$STATE_ROOT/.current.$$
    rm -rf "$temporary"
    snapshot "$temporary/start"
    read_first /proc/sys/kernel/random/boot_id >"$temporary/boot-id"
    chmod -R go-rwx "$temporary"
    rm -rf "$CURRENT"
    mv "$temporary" "$CURRENT"
    echo "[power-audit] baseline recorded at $(cat "$CURRENT/start/unix")"
    echo "[power-audit] now unplug USB, use both applications, then leave the device untouched past its idle timeout"
}

value() {
    sed -n "s/^$2=//p" "$1" 2>/dev/null | sed -n '1p'
}

number_or_zero() {
    case "$1" in ''|*[!0-9]*) printf '0\n' ;; *) printf '%s\n' "$1" ;; esac
}

collect() {
    [ -d "$CURRENT/start" ] || fail 'run begin before collect'
    start_boot=$(cat "$CURRENT/boot-id")
    [ "$start_boot" = "$(read_first /proc/sys/kernel/random/boot_id)" ] \
        || fail 'device rebooted during the audit; the interval is not comparable'
    rm -rf "$CURRENT/end"
    snapshot "$CURRENT/end"
    started=$(cat "$CURRENT/start/unix")
    ended=$(cat "$CURRENT/end/unix")
    duration=$((ended - started))
    start_suspend=$(number_or_zero "$(cat "$CURRENT/start/suspend-success")")
    end_suspend=$(number_or_zero "$(cat "$CURRENT/end/suspend-success")")
    suspend_delta=$((end_suspend - start_suspend))
    failures=0

    echo '[power-audit] interval'
    echo "duration_seconds=$duration"
    echo "suspend_success_delta=$suspend_delta"
    echo "start_wake_locks=$(cat "$CURRENT/start/wake-lock")"
    echo "end_wake_locks=$(cat "$CURRENT/end/wake-lock")"
    echo "start_battery_status=$(cat "$CURRENT/start/battery-status" 2>/dev/null || echo unavailable)"
    echo "start_battery_capacity=$(cat "$CURRENT/start/battery-capacity" 2>/dev/null || echo unavailable)"
    echo "end_battery_status=$(cat "$CURRENT/end/battery-status" 2>/dev/null || echo unavailable)"
    echo "end_battery_capacity=$(cat "$CURRENT/end/battery-capacity" 2>/dev/null || echo unavailable)"

    if [ "$suspend_delta" -lt 1 ]; then
        echo '[power-audit] ERROR: no successful deep-suspend cycle was observed'
        failures=$((failures + 1))
    fi

    echo '[power-audit] unit deltas'
    for unit in $UNITS; do
        safe=$(printf '%s' "$unit" | tr '@.' '__')
        start_file=$CURRENT/start/unit-$safe
        end_file=$CURRENT/end/unit-$safe
        start_restarts=$(number_or_zero "$(value "$start_file" NRestarts)")
        end_restarts=$(number_or_zero "$(value "$end_file" NRestarts)")
        start_cpu=$(number_or_zero "$(value "$start_file" CPUUsageNSec)")
        end_cpu=$(number_or_zero "$(value "$end_file" CPUUsageNSec)")
        echo "$unit active=$(value "$end_file" ActiveState) restarts_delta=$((end_restarts - start_restarts)) cpu_nsec_delta=$((end_cpu - start_cpu))"
        if [ "$end_restarts" -gt "$start_restarts" ]; then
            failures=$((failures + 1))
        fi
    done

    journal=$CURRENT/journal.txt
    journalctl --since="@$started" --until="@$ended" --no-pager \
        -u remagicd.service -u remagic-display-host.service -u remagic-home.service \
        -u remagic-agentd.service -u remagic-background-magicpaper.service \
        -u magicpaper-agent.service \
        -u remagic-app@magicpaper.service -u remagic-app@koreader.service \
        >"$journal" 2>&1 || true
    kernel=$CURRENT/kernel.txt
    journalctl -k --since="@$started" --until="@$ended" --no-pager >"$kernel" 2>&1 || true

    retry_count=$(grep -c 'oracle unavailable\|power lease unavailable' "$journal" || true)
    fatal_count=$(grep -Eic 'panic|segfault|core dumped|watchdog|out of memory|oom-kill' "$journal" || true)
    echo "agent_retry_errors=$retry_count"
    echo "fatal_errors=$fatal_count"
    [ "$retry_count" -eq 0 ] || failures=$((failures + 1))
    [ "$fatal_count" -eq 0 ] || failures=$((failures + 1))

    echo '[power-audit] final ReMagic power snapshot'
    cat "$CURRENT/end/power.json"
    echo '[power-audit] relevant journal'
    cat "$journal"
    echo '[power-audit] kernel power journal'
    grep -Ei 'suspend|resume|wake|battery|charger' "$kernel" || true

    if [ "$failures" -eq 0 ]; then
        echo '[power-audit] PASS'
    else
        echo "[power-audit] FAIL ($failures invariant(s))"
        return 1
    fi
}

case "${1:-}" in
    begin) begin ;;
    collect) collect ;;
    *) echo "usage: $0 begin|collect" >&2; exit 2 ;;
esac
