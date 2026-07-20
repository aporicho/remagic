#!/bin/bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ARCHIVE="$ROOT/dist/remagic-manager-aarch64.tar.gz"
HOST=${REMAGIC_HOST:-10.11.99.1}

if [ ! -f "$ARCHIVE" ]; then
    "$ROOT/scripts/build-bundle.sh"
fi

scp -F /dev/null -O "$ARCHIVE" "root@$HOST:/tmp/remagic-manager-aarch64.tar.gz"
ssh -F /dev/null "root@$HOST" \
    "rm -rf /tmp/remagic-install && mkdir -p /tmp/remagic-install && tar -xzf /tmp/remagic-manager-aarch64.tar.gz -C /tmp/remagic-install && /tmp/remagic-install/remagic-manager/scripts/install-device.sh"
