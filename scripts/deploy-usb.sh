#!/bin/bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' \
    "$ROOT/Cargo.toml" | head -n 1)
PI_RUNTIME=${REMAGIC_PI_RUNTIME_DIR:-$ROOT/dist/pi-runtime}
ARCHIVE="$ROOT/dist/system-release/remagic-system-$VERSION-universal-aarch64.tar.gz"
CHECKSUM="$ARCHIVE.sha256"
HOST=${REMAGIC_HOST:-10.11.99.1}
USB_INTERFACE=${REMAGIC_USB_INTERFACE:-}
USB_ALIAS=${REMAGIC_USB_ALIAS:-remagic-usb}
SSH_OPTIONS=(-F /dev/null)

if [ -n "$USB_INTERFACE" ]; then
    USB_PROXY=$ROOT/scripts/lib/usb-tcp-proxy.py
    [ -x "$USB_PROXY" ] || {
        echo "missing executable USB proxy: $USB_PROXY" >&2
        exit 1
    }
    HOST=${REMAGIC_USB_HOST:-$HOST}
    SSH_OPTIONS+=(
        -o "HostName=$HOST"
        -o "HostKeyAlias=$USB_ALIAS"
        -o "ProxyCommand=$USB_PROXY $USB_INTERFACE %h %p"
        -o ControlMaster=no
        -o ControlPath=none
        -o StrictHostKeyChecking=accept-new
    )
    HOST=remagic-device
fi

if [ "${REMAGIC_SKIP_BUILD:-0}" != 1 ]; then
    if [ ! -x "$PI_RUNTIME/bin/node" ] || [ ! -x "$PI_RUNTIME/bin/pi" ] || \
       [ ! -f "$PI_RUNTIME/runtime.env" ]; then
        env -u REMAGIC_USB_INTERFACE -u REMAGIC_USB_HOST \
            -u REMAGIC_USB_PROXY -u REMAGIC_USB_ALIAS \
            REMAGIC_PI_RUNTIME_OUT="$PI_RUNTIME" \
            "$ROOT/scripts/build-pi-runtime.sh"
    fi
    env -u REMAGIC_USB_INTERFACE -u REMAGIC_USB_HOST \
        -u REMAGIC_USB_PROXY -u REMAGIC_USB_ALIAS \
        REMAGIC_PI_RUNTIME_DIR="$PI_RUNTIME" \
        "$ROOT/scripts/build-system-release.sh"
elif [ ! -f "$ARCHIVE" ] || [ ! -f "$CHECKSUM" ]; then
    echo "REMAGIC_SKIP_BUILD=1 requested, but no complete system release exists" >&2
    exit 1
fi

(
    cd "$(dirname -- "$ARCHIVE")"
    sha256sum -c "$(basename -- "$CHECKSUM")"
)

scp "${SSH_OPTIONS[@]}" -O "$ARCHIVE" "$CHECKSUM" "root@$HOST:/tmp/"
ssh "${SSH_OPTIONS[@]}" "root@$HOST" \
    'set -eu; archive=/tmp/'"$(basename -- "$ARCHIVE")"'; checksum=$archive.sha256; txn=/tmp/remagic-install.$$.new; cleanup() { rm -rf "$txn" "$archive" "$checksum"; }; trap cleanup EXIT HUP INT TERM; cd /tmp; sha256sum -c "$(basename "$checksum")"; mkdir "$txn"; tar -xzf "$archive" -C "$txn"; "$txn/remagic-system/install-device.sh"'
