#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TARGET=aarch64-unknown-linux-gnu
SDK=${RM_SDK:-/home/aporicho/rm-sdk-chiappa-3.27}
QUILL=${QUILL_DIR:-$ROOT/../quill-move}
STORE=${REMAGIC_STORE_DIR:-$ROOT/../remagic-store}
UI_FONT=${REMAGIC_UI_FONT:-/home/aporicho/Downloads/方正屏显雅宋.TTF}
PI_RUNTIME=${REMAGIC_PI_RUNTIME_DIR:-}
VERSION=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\([^"]*\)"/\1/p' \
    "$ROOT/Cargo.toml" | head -n 1)
RELEASE_SEQUENCE=${REMAGIC_RELEASE_SEQUENCE:-1}
OUT_ROOT=$ROOT/dist/system-release
RELEASE_ROOT=$OUT_ROOT/remagic-system
PAYLOAD=$RELEASE_ROOT/payload
ARCHIVE=$OUT_ROOT/remagic-system-$VERSION-universal-aarch64.tar.gz
BUILD_ROOT=$(mktemp -d /tmp/remagic-system-build.XXXXXX)
trap 'rm -rf "$BUILD_ROOT"' EXIT

[ -n "$VERSION" ] || { echo "cannot determine ReMagic version" >&2; exit 1; }
[ -s "$ROOT/runtime/pi/extensions/remagic-tools.js" ] && \
    [ -x "$ROOT/scripts/remagic-configure-provider" ] || {
    echo "ReMagic Pi safety extension or provider helper is missing" >&2
    exit 1
}
[ -n "$PI_RUNTIME" ] && [ -d "$PI_RUNTIME" ] && [ ! -L "$PI_RUNTIME" ] || {
    echo "REMAGIC_PI_RUNTIME_DIR must name a self-contained Pi runtime" >&2
    exit 1
}
[ -x "$PI_RUNTIME/bin/node" ] && [ -x "$PI_RUNTIME/bin/pi" ] && \
    [ -f "$PI_RUNTIME/runtime.env" ] || {
    echo "Pi runtime must contain executable bin/node, bin/pi, and runtime.env" >&2
    exit 1
}
pi_runtime_schema=$(sed -n 's/^REMAGIC_PI_RUNTIME_SCHEMA=//p' \
    "$PI_RUNTIME/runtime.env" | sed -n '1p')
pi_version=$(sed -n 's/^REMAGIC_PI_VERSION=//p' \
    "$PI_RUNTIME/runtime.env" | sed -n '1p')
node_version=$(sed -n 's/^REMAGIC_NODE_VERSION=//p' \
    "$PI_RUNTIME/runtime.env" | sed -n '1p')
[ "$pi_runtime_schema" = 1 ] && \
    [[ "$pi_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][A-Za-z0-9.-]+)?$ ]] && \
    [[ "$node_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][A-Za-z0-9.-]+)?$ ]] || {
    echo "Pi runtime version manifest is invalid" >&2
    exit 1
}
[ -d "$STORE" ] || { echo "ReMagic Store repository is missing: $STORE" >&2; exit 1; }
[ -d "$QUILL/src" ] && [ -f "$QUILL/vendor/libqsgepaper.so" ] || {
    echo "Quill source/vendor ABI library is incomplete: $QUILL" >&2
    exit 1
}
[ -s "$UI_FONT" ] && [ ! -L "$UI_FONT" ] || {
    echo "ReMagic UI font is missing or unsafe: $UI_FONT" >&2
    exit 1
}
ENV_FILE=$(find "$SDK" -maxdepth 1 -name 'environment-setup-*' -print -quit)
[ -n "$ENV_FILE" ] || { echo "reMarkable SDK was not found under $SDK" >&2; exit 1; }

if [ "${REMAGIC_SKIP_CHECKS:-0}" != 1 ]; then
    "$ROOT/scripts/check.sh"
    (cd "$STORE" && cargo fmt --all --check && cargo test --workspace --all-targets && \
        cargo clippy --workspace --all-targets -- -D warnings)
fi

unset LD_LIBRARY_PATH
# shellcheck disable=SC1090
source "$ENV_FILE"
compiler=$(command -v "${CC%% *}")
cxx=$(command -v "${CXX%% *}")
strip_tool=$(command -v "${STRIP%% *}")
sysroot=${SDKTARGETSYSROOT:-${OECORE_TARGET_SYSROOT:-}}
[ -n "$compiler" ] && [ -n "$cxx" ] && [ -n "$strip_tool" ] && \
    [ -d "$sysroot" ] || {
    echo "reMarkable SDK compiler/sysroot contract is incomplete" >&2
    exit 1
}

export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=$compiler
export CC_aarch64_unknown_linux_gnu=$compiler
export CFLAGS_aarch64_unknown_linux_gnu="-O2 -pipe --sysroot=$sysroot -march=armv8-a"
export RUSTFLAGS="-C target-cpu=generic -C link-arg=--sysroot=$sysroot -C link-arg=-march=armv8-a"

# Build the one small display ABI bridge at the baseline ARMv8-A ISA. The
# linked vendor library is used only for symbol resolution and is not shipped.
mkdir -p "$BUILD_ROOT/quill"
qt_include=$sysroot/usr/include
"$cxx" --sysroot="$sysroot" -march=armv8-a -fPIC -shared -O2 -std=c++17 \
    -I "$qt_include" -I "$qt_include/QtCore" -I "$qt_include/QtGui" \
    "$QUILL/src/vendor_probe.cpp" "$QUILL/src/quill_c.cpp" \
    -L "$QUILL/vendor" -lqsgepaper -lQt6Gui -lQt6Core -ldl \
    -o "$BUILD_ROOT/quill/libquill.so"

cd "$ROOT"
cargo build --locked --release --target "$TARGET" --workspace
cargo build --locked --release --target "$TARGET" -p remagic-home --features device
(
    cd native/remagic-display-host
    RUSTFLAGS="$RUSTFLAGS -C link-arg=-Wl,-rpath-link,$QUILL/vendor" \
    QUILL_LIB_DIR="$BUILD_ROOT/quill" \
        cargo build --locked --release --target "$TARGET" --features device
)

# KOReader's official remarkable build needs only this compatibility client;
# no AppLoad executable, QML or second lifecycle manager is included.
env CC="$compiler" CXX="$cxx" \
    CFLAGS="--sysroot=$sysroot -march=armv8-a" \
    CXXFLAGS="--sysroot=$sysroot -march=armv8-a" \
    STRIP="$strip_tool" \
    "$ROOT/native/appload-runtime/build-qtfb-shim.sh" "$BUILD_ROOT/shims"

RM_SDK="$SDK" "$STORE/scripts/build-remagic.sh"
store_binary=$STORE/target/$TARGET/release/remagic-store
store_out=$BUILD_ROOT/store-package
REMAGIC_STORE_BIN="$store_binary" REMAGIC_STORE_SKIP_BUILD=1 OUT_DIR="$store_out" \
    "$STORE/scripts/build-remagic-package.sh" >/dev/null
store_archive=$(find "$store_out" -maxdepth 1 -type f -name 'remagic-store-*.tar.gz' \
    -print -quit)
[ -n "$store_archive" ] || { echo "Store system package was not built" >&2; exit 1; }

rm -rf "$OUT_ROOT"
mkdir -p "$PAYLOAD/bin" "$PAYLOAD/lib" "$PAYLOAD/fonts" \
    "$PAYLOAD/shims" "$PAYLOAD/libexec" "$PAYLOAD/share/systemd" \
    "$PAYLOAD/share/testing/manifests" "$RELEASE_ROOT/packages"
for binary in remagicd remagic-home remagic-runner remagicctl \
    remagic-vellum-worker remagic-package-inspect remagic-agentd remagic-update; do
    install -m 0755 "$ROOT/target/$TARGET/release/$binary" "$PAYLOAD/bin/$binary"
done
install -m 0755 \
    "$ROOT/native/remagic-display-host/target/$TARGET/release/remagic-display-host" \
    "$PAYLOAD/bin/remagic-display-host"
install -m 0644 "$BUILD_ROOT/quill/libquill.so" "$PAYLOAD/lib/libquill.so"
"$strip_tool" --strip-unneeded "$PAYLOAD/lib/libquill.so"
install -m 0644 "$UI_FONT" "$PAYLOAD/fonts/UIFont.ttf"
mkdir -p "$PAYLOAD/runtime/pi"
cp -aL "$PI_RUNTIME/." "$PAYLOAD/runtime/pi/"
[ -z "$(find "$PAYLOAD/runtime/pi" -type l -print -quit)" ] || {
    echo "canonical Pi release runtime still contains symbolic links" >&2
    exit 1
}
# The official Node archive carries debug/symbol sections. The SDK's AArch64
# strip tool removes only link-time metadata from the release copy; the source
# runtime remains untouched and its JavaScript/native modules are unchanged.
"$strip_tool" --strip-unneeded "$PAYLOAD/runtime/pi/bin/node"
mkdir -p "$PAYLOAD/runtime/pi/extensions"
install -m 0644 "$ROOT/runtime/pi/extensions/remagic-tools.js" \
    "$PAYLOAD/runtime/pi/extensions/remagic-tools.js"

# Execute the exact stripped ARM64 Node/Pi payload under the SDK emulator. A
# format check alone cannot catch a broken loader, missing JS dependency, or an
# extension that Pi cannot import. `get_state` is local and makes no API call.
qemu_tool=$(command -v qemu-aarch64 || true)
if [ -z "$qemu_tool" ]; then
    qemu_tool=$(find "$SDK/sysroots" -type f -name qemu-aarch64 -print -quit)
fi
[ -x "$qemu_tool" ] || { echo "SDK AArch64 emulator is unavailable" >&2; exit 1; }
pi_probe=$(
    printf '%s\n' '{"id":"release-arm-state","type":"get_state"}' | \
        env HOME="$BUILD_ROOT/pi-probe-home" \
            PI_CODING_AGENT_DIR="$BUILD_ROOT/pi-probe-config" \
            PI_SKIP_VERSION_CHECK=1 PI_TELEMETRY=0 \
            DEEPSEEK_API_KEY=release-probe-only \
            "$qemu_tool" -L "$sysroot" "$PAYLOAD/runtime/pi/bin/node" \
            "$PAYLOAD/runtime/pi/node_modules/@earendil-works/pi-coding-agent/dist/cli.js" \
            --mode rpc --provider deepseek --model deepseek-v4-flash \
            --thinking off --no-session --no-skills --no-prompt-templates \
            --no-context-files --no-approve --system-prompt paper \
            --no-builtin-tools --no-extensions --extension \
            "$PAYLOAD/runtime/pi/extensions/remagic-tools.js"
)
printf '%s\n' "$pi_probe" | grep -Fq \
    '"id":"release-arm-state","type":"response","command":"get_state","success":true' || {
    echo "packaged ARM64 Pi runtime failed its RPC startup probe" >&2
    exit 1
}
install -m 0755 "$BUILD_ROOT/shims/qtfb-shim.so" "$PAYLOAD/shims/qtfb-shim.so"
install -m 0644 "$BUILD_ROOT/shims/LICENSE.qtfb-shim" \
    "$PAYLOAD/shims/LICENSE.qtfb-shim"

for helper in remagic-register remagic-recover remagic-configure-provider; do
    install -m 0755 "$ROOT/scripts/$helper" "$PAYLOAD/libexec/$helper"
done
install -m 0644 "$ROOT/scripts/system-release/system-trusted-keys.json" \
    "$PAYLOAD/share/system-trusted-keys.json"
for helper in deployment-common.sh device-test-recovery.sh device-test-manifests.sh \
    device-test-isolation.sh; do
    install -m 0644 "$ROOT/scripts/lib/$helper" "$PAYLOAD/libexec/$helper"
done
for unit in remagicd.service remagic-display-host.service remagic-home.service \
    remagic-app@.service remagic-recover.service remagic-agentd.service \
    remagic-agentd.socket; do
    install -m 0644 "$ROOT/systemd/$unit" "$PAYLOAD/share/systemd/$unit"
done
mkdir -p "$PAYLOAD/share/systemd/remagic-app@koreader.service.d"
install -m 0644 \
    "$ROOT/systemd/remagic-app@koreader.service.d/10-koreader-runtime.conf" \
    "$PAYLOAD/share/systemd/remagic-app@koreader.service.d/10-koreader-runtime.conf"
for test_script in device-acceptance-v2.sh device-fault-acceptance-v2.sh \
    device-stress-acceptance-v2.sh device-lock-acceptance-v2.sh; do
    install -m 0755 "$ROOT/scripts/$test_script" "$PAYLOAD/share/testing/$test_script"
done
for test_manifest in magicpaper.toml koreader.toml; do
    install -m 0644 "$ROOT/testing/manifests/$test_manifest" \
        "$PAYLOAD/share/testing/manifests/$test_manifest"
done

store_name=$(basename "$store_archive")
install -m 0644 "$store_archive" "$RELEASE_ROOT/packages/$store_name"
install -m 0755 "$ROOT/scripts/system-release/install-device.sh" \
    "$RELEASE_ROOT/install-device.sh"
install -m 0644 "$ROOT/scripts/system-release/common.sh" "$RELEASE_ROOT/common.sh"
cat >"$RELEASE_ROOT/release.env" <<EOF
REMAGIC_RELEASE_SCHEMA=1
REMAGIC_VERSION=$VERSION
REMAGIC_RELEASE_SEQUENCE=$RELEASE_SEQUENCE
REMAGIC_API=5
REMAGIC_PI_RUNTIME_SCHEMA=$pi_runtime_schema
REMAGIC_PI_VERSION=$pi_version
REMAGIC_NODE_VERSION=$node_version
SUPPORTED_OS_SERIES=3.27
SUPPORTED_DEVICES=ferrari,chiappa
STORE_PACKAGE=packages/$store_name
EOF

(
    cd "$PAYLOAD"
    find . -type f ! -path './share/system-files.sha256' -print0 \
        | LC_ALL=C sort -z | xargs -0 sha256sum >share/system-files.sha256
)
(
    cd "$RELEASE_ROOT"
    find . -type f ! -name SHA256SUMS -print0 \
        | LC_ALL=C sort -z | xargs -0 sha256sum >SHA256SUMS
)

for artifact in "$PAYLOAD/bin/"* "$PAYLOAD/lib/libquill.so" \
    "$PAYLOAD/runtime/pi/bin/node" \
    "$PAYLOAD/shims/qtfb-shim.so" "$store_binary"; do
    file "$artifact" | grep -q 'ELF 64-bit.*ARM aarch64' || {
        echo "refusing non-AArch64 system artifact: $artifact" >&2
        exit 1
    }
done
readelf -d "$PAYLOAD/bin/remagic-display-host" | grep -q 'Shared library: \[libquill.so\]' || {
    echo "display host has no libquill runtime dependency" >&2
    exit 1
}

tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner \
    -C "$OUT_ROOT" -cf - remagic-system | gzip -n >"$ARCHIVE"
sha256sum "$ARCHIVE" >"$ARCHIVE.sha256"
archive_sha=$(sha256sum "$ARCHIVE" | awk '{print $1}')
cat >"$OUT_ROOT/remagic-release.env" <<EOF
REMAGIC_RELEASE_SCHEMA=1
REMAGIC_VERSION=$VERSION
SUPPORTED_OS_SERIES=3.27
SUPPORTED_DEVICES=ferrari,chiappa
ARCHIVE_URL=https://github.com/aporicho/remagic/releases/download/v$VERSION/$(basename "$ARCHIVE")
ARCHIVE_SHA256=$archive_sha
EOF
"$ROOT/scripts/system-release/create-release-metadata.sh" \
    "$ARCHIVE" "$VERSION" "$RELEASE_SEQUENCE" \
    "$OUT_ROOT/remagic-release-v1.json" >/dev/null
printf '%s\n' "$ARCHIVE"
printf '%s  %s\n' "$archive_sha" "$ARCHIVE"
