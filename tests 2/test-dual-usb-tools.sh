#!/bin/bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d /tmp/remagic-dual-usb-test.XXXXXX)
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT HUP INT TERM

# A caller such as `paper-pro deploy` legitimately exports its selected real
# interface. This fixture must remain hermetic when the complete check suite is
# run inside that deployment process.
unset REMAGIC_USB_INTERFACE REMAGIC_USB_HOST REMAGIC_USB_PROXY REMAGIC_USB_ALIAS

PYTHONPYCACHEPREFIX=$TMP/pycache python3 -m py_compile \
    "$ROOT/scripts/lib/usb-tcp-proxy.py"
"$ROOT/scripts/paper-pro" --help | grep -q 'Paper Pro' || true
"$ROOT/scripts/paper-pro-move" --help | grep -q 'install'

for name in REMAGIC_USB_INTERFACE REMAGIC_USB_HOST REMAGIC_USB_PROXY REMAGIC_USB_ALIAS; do
    grep -q -- "-u $name" "$ROOT/scripts/deploy-usb.sh" || {
        echo "deploy build leaks $name into host checks" >&2
        exit 1
    }
done

DEVICE_LABEL='fixture'
DEVICE_MACHINE='reMarkable Ferrari'
DEVICE_ALIAS='fixture-usb'
# shellcheck source=scripts/lib/usb-device.sh
. "$ROOT/scripts/lib/usb-device.sh"

SSH_CAPTURE=$TMP/probe-ssh-arguments
ssh() {
    printf '%s\n' "$@" >"$SSH_CAPTURE"
    printf '%s\n' 'reMarkable Ferrari'
}
[ "$(usb_probe_machine usb-pro)" = 'reMarkable Ferrari' ]
grep -qx 'UserKnownHostsFile=/dev/null' "$SSH_CAPTURE"
grep -qx 'GlobalKnownHostsFile=/dev/null' "$SSH_CAPTURE"
grep -qx 'StrictHostKeyChecking=no' "$SSH_CAPTURE"
if grep -q '^HostKeyAlias=' "$SSH_CAPTURE"; then
    echo 'discovery probe persisted a host key against a USB port' >&2
    exit 1
fi

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
