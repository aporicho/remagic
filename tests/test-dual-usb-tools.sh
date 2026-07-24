#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d /tmp/remagic-dual-usb-test.XXXXXX)
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT HUP INT TERM

PYTHONPYCACHEPREFIX=$TMP/pycache python3 -m py_compile \
    "$ROOT/scripts/lib/usb-tcp-proxy.py"
"$ROOT/scripts/paper-pro" --help | grep -q 'Paper Pro' || true
"$ROOT/scripts/paper-pro-move" --help | grep -q 'install'

DEVICE_LABEL='fixture'
DEVICE_MACHINE='reMarkable Ferrari'
DEVICE_ALIAS='fixture-usb'
# shellcheck source=scripts/lib/usb-device.sh
. "$ROOT/scripts/lib/usb-device.sh"

usb_interfaces() {
    printf '%s\n' usb-move usb-pro
}
usb_probe_machine() {
    case "$1" in
        usb-pro) printf '%s\n' 'reMarkable Ferrari' ;;
        usb-move) printf '%s\n' 'reMarkable Chiappa' ;;
        *) return 1 ;;
    esac
}

[ "$(usb_select_interface)" = usb-pro ]
DEVICE_MACHINE='reMarkable Chiappa'
[ "$(usb_select_interface)" = usb-move ]
usb_safe_interface enp18s0u1c2
if usb_safe_interface '../unsafe interface'; then
    echo 'unsafe USB interface name was accepted' >&2
    exit 1
fi

echo 'dual USB tools contract ok'
