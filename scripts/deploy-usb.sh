#!/bin/bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ARCHIVE="$ROOT/dist/remagic-aarch64.tar.gz"
CHECKSUM="$ARCHIVE.sha256"
HOST=${REMAGIC_HOST:-10.11.99.1}

if [ "${REMAGIC_SKIP_BUILD:-0}" != 1 ]; then
    "$ROOT/scripts/build-bundle.sh"
elif [ ! -f "$ARCHIVE" ] || [ ! -f "$CHECKSUM" ]; then
    echo "REMAGIC_SKIP_BUILD=1 requested, but no complete bundle exists" >&2
    exit 1
fi

(
    cd "$ROOT/dist"
    sha256sum -c remagic-aarch64.tar.gz.sha256
)

scp -F /dev/null -O "$ARCHIVE" "$CHECKSUM" "root@$HOST:/tmp/"
ssh -F /dev/null "root@$HOST" \
    'set -eu; txn=/tmp/remagic-install.$$.new; cleanup() { rm -rf "$txn"; }; trap cleanup EXIT HUP INT TERM; cd /tmp; sha256sum -c remagic-aarch64.tar.gz.sha256; mkdir "$txn"; tar -xzf /tmp/remagic-aarch64.tar.gz -C "$txn"; "$txn/remagic/scripts/install-device.sh"'
