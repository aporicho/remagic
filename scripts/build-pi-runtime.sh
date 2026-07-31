#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
PI_MANIFEST=$ROOT/runtime/pi/package.json
PI_LOCK=$ROOT/runtime/pi/package-lock.json
PI_VERSION=$(sed -n \
    's/.*"@earendil-works\/pi-coding-agent": "\([^"]*\)".*/\1/p' \
    "$PI_MANIFEST")
NODE_VERSION=${REMAGIC_NODE_VERSION:-22.23.1}
OUT=${REMAGIC_PI_RUNTIME_OUT:-$ROOT/dist/pi-runtime}
BUILD=$(mktemp -d /tmp/remagic-pi-runtime.XXXXXX)
trap 'rm -rf "$BUILD"' EXIT

case $OUT in
    ''|/) echo "unsafe Pi runtime output path" >&2; exit 2 ;;
esac
[ -n "$PI_VERSION" ] && [ -f "$PI_LOCK" ] || {
    echo "pinned Pi package manifest/lock is unavailable" >&2
    exit 1
}
archive=node-v$NODE_VERSION-linux-arm64.tar.xz
base=https://nodejs.org/dist/v$NODE_VERSION
curl -fsSL "$base/$archive" -o "$BUILD/$archive"
expected=$(curl -fsSL "$base/SHASUMS256.txt" | awk -v name="$archive" '$2 == name {print $1}')
[ -n "$expected" ] || { echo "Node checksum is unavailable" >&2; exit 1; }
printf '%s  %s\n' "$expected" "$BUILD/$archive" | sha256sum -c -
tar -xJf "$BUILD/$archive" -C "$BUILD"

stage=$BUILD/runtime
mkdir -p "$stage/bin"
install -m 0755 "$BUILD/node-v$NODE_VERSION-linux-arm64/bin/node" "$stage/bin/node"
install -m 0644 "$PI_MANIFEST" "$PI_LOCK" "$stage/"
(
    cd "$stage"
    npm_config_cpu=arm64 npm_config_os=linux npm ci \
        --ignore-scripts --omit=dev --no-audit --no-fund
)
# Published source maps and TypeScript declaration files are useful to library
# developers but never participate in Node execution. Removing only these
# files saves roughly 60 MiB and about 10,000 installation checksum entries,
# while retaining JavaScript, source files, licenses, and every native asset.
find "$stage/node_modules" -type f -name '*.map' -delete
find "$stage/node_modules" -type f \
    \( -name '*.d.ts' -o -name '*.d.mts' -o -name '*.d.cts' \) -delete
cli=$stage/node_modules/@earendil-works/pi-coding-agent/dist/cli.js
[ -f "$cli" ] || { echo "installed Pi package has no RPC CLI" >&2; exit 1; }
printf '%s\n' \
    '#!/bin/sh' \
    'ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)' \
    'exec "$ROOT/bin/node" "$ROOT/node_modules/@earendil-works/pi-coding-agent/dist/cli.js" "$@"' \
    >"$stage/bin/pi"
chmod 0755 "$stage/bin/pi"
printf '%s\n' \
    'REMAGIC_PI_RUNTIME_SCHEMA=1' \
    "REMAGIC_PI_VERSION=$PI_VERSION" \
    "REMAGIC_NODE_VERSION=$NODE_VERSION" \
    >"$stage/runtime.env"
chmod 0644 "$stage/runtime.env"

rm -rf "$OUT.new"
mkdir -p "$(dirname -- "$OUT")"
mv "$stage" "$OUT.new"
rm -rf "$OUT"
mv "$OUT.new" "$OUT"
printf 'Pi %s / Node %s runtime: %s\n' "$PI_VERSION" "$NODE_VERSION" "$OUT"
