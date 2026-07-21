#!/bin/bash
set -euo pipefail

# Build only the small QTFB compatibility client used by KOReader. Remagic's
# display ownership, application supervision and home UI are native services;
# the AppLoad executable/QML runtime is deliberately not part of the bundle.
ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
OUTPUT=${1:?usage: build-qtfb-shim.sh OUTPUT_DIRECTORY}
ARCHIVE=/tmp/rm-appload-v0.5.3.tar.gz
URL=https://github.com/asivery/rm-appload/archive/refs/tags/v0.5.3.tar.gz
EXPECTED=df5878cf06e1167b9156c2455bab762366d2a4b2058179073734d9ca72e46e94

if [ ! -f "$ARCHIVE" ]; then
    curl -fL --retry 3 -o "$ARCHIVE" "$URL"
fi
printf '%s  %s\n' "$EXPECTED" "$ARCHIVE" | sha256sum -c -

BUILD_ROOT=$(mktemp -d /tmp/remagic-qtfb-shim.XXXXXX)
trap 'rm -rf "$BUILD_ROOT"' EXIT
bsdtar -xf "$ARCHIVE" -C "$BUILD_ROOT" --strip-components 1
# This baseline patch contains the standalone-socket QTFB connection change.
# Later patches modify only the retired AppLoad runtime and are not applied.
patch --batch --forward -p1 -d "$BUILD_ROOT" \
    < "$ROOT/patches/0001-remagic-runtime.patch"

SHIM_BUILD="$BUILD_ROOT/build-remagic-shim"
cmake -S "$BUILD_ROOT/shim" -B "$SHIM_BUILD" -DCMAKE_BUILD_TYPE=Release
cmake --build "$SHIM_BUILD" --target qtfb-shim --parallel "$(nproc)"
if ! file "$SHIM_BUILD/qtfb-shim.so" | grep -q 'ARM aarch64'; then
    echo "build-qtfb-shim.sh: refusing non-AArch64 device artifact" >&2
    file "$SHIM_BUILD/qtfb-shim.so" >&2
    exit 1
fi
if [ -n "${STRIP:-}" ]; then
    "$STRIP" --strip-unneeded "$SHIM_BUILD/qtfb-shim.so"
fi

mkdir -p "$OUTPUT"
cp "$SHIM_BUILD/qtfb-shim.so" "$OUTPUT/qtfb-shim.so"
cp "$BUILD_ROOT/LICENSE" "$OUTPUT/LICENSE.qtfb-shim"
