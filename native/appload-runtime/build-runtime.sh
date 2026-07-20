#!/bin/bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
OUTPUT=${1:?usage: build-runtime.sh OUTPUT_DIRECTORY}
ARCHIVE=/tmp/rm-appload-v0.5.3.tar.gz
URL=https://github.com/asivery/rm-appload/archive/refs/tags/v0.5.3.tar.gz
EXPECTED=df5878cf06e1167b9156c2455bab762366d2a4b2058179073734d9ca72e46e94
PATCHES=(
    "$ROOT/patches/0001-remagic-runtime.patch"
    "$ROOT/patches/0002-remagic-lifecycle-p0.patch"
    "$ROOT/patches/0003-remagic-atomic-app-switch.patch"
    "$ROOT/patches/0004-remagic-vendor-refresh-abi.patch"
    "$ROOT/patches/0005-remagic-raw-pen-bridge.patch"
    "$ROOT/patches/0006-remagic-koreader-lifecycle.patch"
    "$ROOT/patches/0007-remagic-runtime-socket-reentrancy.patch"
    "$ROOT/patches/0008-remagic-signalfd-shutdown.patch"
)

if [ ! -f "$ARCHIVE" ]; then
    curl -fL --retry 3 -o "$ARCHIVE" "$URL"
fi
printf '%s  %s\n' "$EXPECTED" "$ARCHIVE" | sha256sum -c -

BUILD_ROOT=$(mktemp -d /tmp/remagic-appload-build.XXXXXX)
trap 'rm -rf "$BUILD_ROOT"' EXIT
bsdtar -xf "$ARCHIVE" -C "$BUILD_ROOT" --strip-components 1
for patch_file in "${PATCHES[@]}"; do
    patch --batch --forward -p1 -d "$BUILD_ROOT" < "$patch_file"
done
cp "$ROOT/_start.qml" "$BUILD_ROOT/_start.qml"

assert_patched_source() {
    pattern=$1
    file=$2
    description=$3
    if ! grep -Fq "$pattern" "$file"; then
        echo "build-runtime.sh: patched source assertion failed: $description" >&2
        exit 1
    fi
}

# Process-directed TERM/INT must be blocked before plugin or Qt initialization,
# then consumed on the GUI thread. Managed children explicitly restore normal
# signal delivery before exec so fallback termination remains reliable.
assert_patched_source 'if (!blockTerminationSignals())' "$BUILD_ROOT/src/main.cpp" \
    'termination signals are not blocked before runtime initialization'
assert_patched_source '::signalfd(' "$BUILD_ROOT/src/main.cpp" \
    'signalfd termination dispatch is missing'
assert_patched_source '::sigprocmask(SIG_UNBLOCK' "$BUILD_ROOT/src/libraryexternals.cpp" \
    'managed child processes do not unblock TERM/INT'
assert_patched_source '_shutdownElapsed.elapsed() >= 5500' "$BUILD_ROOT/src/RuntimeControl.cpp" \
    'KOReader graceful shutdown force deadline regressed'
assert_patched_source '_shutdownElapsed.elapsed() < 6500' "$BUILD_ROOT/src/RuntimeControl.cpp" \
    'runtime teardown fence deadline regressed'

block_line=$(grep -nF 'if (!blockTerminationSignals())' "$BUILD_ROOT/src/main.cpp" | cut -d: -f1)
plugin_line=$(grep -nF '    loadTestingModules();' "$BUILD_ROOT/src/main.cpp" | cut -d: -f1)
qt_line=$(grep -nF '    QGuiApplication application(argc, argv);' "$BUILD_ROOT/src/main.cpp" | cut -d: -f1)
if [ "$block_line" -ge "$plugin_line" ] || [ "$block_line" -ge "$qt_line" ]; then
    echo 'build-runtime.sh: TERM/INT must be blocked before plugins and QGuiApplication' >&2
    exit 1
fi
if grep -Fq '::sigaction(SIGTERM' "$BUILD_ROOT/src/main.cpp"; then
    echo 'build-runtime.sh: legacy SIGTERM handler bypasses the signalfd shutdown fence' >&2
    exit 1
fi

APP_BUILD="$BUILD_ROOT/build-remagic-runtime"
SHIM_BUILD="$BUILD_ROOT/build-remagic-shim"
mkdir -p "$APP_BUILD" "$SHIM_BUILD"
(
    cd "$APP_BUILD"
    qmake6 "$BUILD_ROOT/appload.pro"
    make -j"$(nproc)"
)
cmake -S "$BUILD_ROOT/shim" -B "$SHIM_BUILD" -DCMAKE_BUILD_TYPE=Release
cmake --build "$SHIM_BUILD" --target qtfb-shim --parallel "$(nproc)"

for artifact in "$APP_BUILD/appload" "$SHIM_BUILD/qtfb-shim.so"; do
    if ! file "$artifact" | grep -q 'ARM aarch64'; then
        echo "build-runtime.sh: refusing non-AArch64 device artifact: $artifact" >&2
        file "$artifact" >&2
        exit 1
    fi
done

mkdir -p "$OUTPUT/applications_root" "$OUTPUT/shims"
cp "$APP_BUILD/appload" "$OUTPUT/remagic-appload-runtime"
cp "$SHIM_BUILD/qtfb-shim.so" "$OUTPUT/shims/qtfb-shim.so"
cp "$BUILD_ROOT/_start.qml" "$OUTPUT/_start.qml"
cp "$BUILD_ROOT/LICENSE" "$OUTPUT/LICENSE.appload"
