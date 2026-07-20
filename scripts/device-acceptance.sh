#!/bin/sh
set -eu

CTL=${REMAGIC_CTL:-/home/root/apps/remagic/bin/remagicctl}

wait_state() {
    want=$1
    i=0
    while [ "$i" -lt 100 ]; do
        out=$($CTL status 2>/dev/null || true)
        case "$want" in
            manager) printf '%s' "$out" | grep -q '"domain": "manager"' && return 0 ;;
            system) printf '%s' "$out" | grep -q '"domain": "system"' && return 0 ;;
            foreground) printf '%s' "$out" | grep -q '"foreground":' && return 0 ;;
        esac
        sleep 0.25
        i=$((i + 1))
    done
    echo "timeout waiting for state: $want" >&2
    $CTL status >&2 || true
    return 1
}

wait_service() {
    unit=$1
    want=$2
    i=0
    while [ "$i" -lt 100 ]; do
        [ "$(systemctl is-active "$unit" 2>/dev/null || true)" = "$want" ] && return 0
        sleep 0.25
        i=$((i + 1))
    done
    echo "timeout waiting for $unit=$want" >&2
    systemctl is-active "$unit" >&2 || true
    return 1
}

assert_service() {
    wait_service "$1" "$2"
}

# Normalize a manually interrupted run before starting the matrix.
if $CTL status | grep -q '"foreground":'; then
    $CTL park >/dev/null
    wait_state manager
fi
if $CTL status | grep -q '"domain": "system"'; then
    $CTL manager >/dev/null
    wait_state manager
fi
wait_state manager
wait_service remagic-home.service active

$CTL launch magicpaper >/dev/null
wait_state foreground
assert_service remagic-app@magicpaper.service active
assert_service remagic-home.service inactive

$CTL park >/dev/null
wait_state manager
assert_service remagic-app@magicpaper.service inactive
wait_service remagic-home.service active

$CTL close magicpaper --complete >/dev/null
wait_service magicpaper-agent.service inactive

# Verify a parked/closed app can be launched again and closed again.
$CTL launch magicpaper >/dev/null
wait_state foreground
$CTL park >/dev/null
wait_state manager
$CTL close magicpaper --complete >/dev/null
wait_service magicpaper-agent.service inactive

cycle=1
while [ "$cycle" -le 3 ]; do
    $CTL launch koreader >/dev/null
    wait_state foreground
    assert_service remagic-app@koreader.service active
    assert_service remagic-home.service inactive
    # Service state alone is insufficient: the adapter must have connected
    # KOReader to the real einkface SHM display bridge.
    journalctl -u remagic-app@koreader.service -n 120 --no-pager \
        | grep -q 'SHM initialized:'
    $CTL park >/dev/null
    wait_state manager
    assert_service remagic-app@koreader.service inactive
    wait_service remagic-home.service active
    # The vendor system host and Quill may legitimately recreate the lock
    # while changing display ownership. The meaningful assertion is that the
    # next KOReader launch acquires its SHM bridge successfully.
    cycle=$((cycle + 1))
done
$CTL close koreader --complete >/dev/null

$CTL system >/dev/null
wait_state system
assert_service xochitl.service active
assert_service paperweight.service active
assert_service remagic-home.service inactive

if systemctl is-failed --quiet remagic-home.service; then
    echo "remagic-home is failed" >&2
    exit 1
fi
if systemctl is-failed --quiet remagicd.service; then
    echo "remagicd is failed" >&2
    exit 1
fi

echo ACCEPTANCE_OK
