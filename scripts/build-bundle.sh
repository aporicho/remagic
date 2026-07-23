#!/bin/bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TARGET=aarch64-unknown-linux-gnu
OUT="$ROOT/dist/remagic"
SDK=${RM_SDK:-/home/aporicho/rm-sdk-chiappa-3.27}
QUILL=${QUILL_DIR:-$ROOT/../quill-move}
MAGICPAPER=${MAGICPAPER_DIR:-$ROOT/../magicpaper}
KOREADER_ADAPTER=${KOREADER_ADAPTER_DIR:-$ROOT/../koreader-for-remagic}
KOREADER_RELEASE_CONTRACT=$ROOT/scripts/lib/koreader-release.sh
MAGICPAPER_FONT_CONTRACT=$ROOT/scripts/lib/magicpaper-font-contract.sh
[ -r "$KOREADER_RELEASE_CONTRACT" ] || {
    echo "KOReader release contract is missing: $KOREADER_RELEASE_CONTRACT" >&2
    exit 1
}
[ -r "$MAGICPAPER_FONT_CONTRACT" ] || {
    echo "MagicPaper font contract is missing: $MAGICPAPER_FONT_CONTRACT" >&2
    exit 1
}
# shellcheck source=scripts/lib/magicpaper-font-contract.sh
. "$MAGICPAPER_FONT_CONTRACT"
# shellcheck source=scripts/lib/koreader-release.sh
. "$KOREADER_RELEASE_CONTRACT"
MAGICPAPER_BUTTER_FONT_ARCHIVE=${MAGICPAPER_BUTTER_FONT_ARCHIVE:-${HOME:?}/Downloads/黄油拾叁体.zip}
MAGICPAPER_851_FONT_ARCHIVE=${MAGICPAPER_851_FONT_ARCHIVE:-${HOME:?}/Downloads/851远星夜行手写体.zip}
readonly MAGICPAPER_BUTTER_ARCHIVE_SHA256=eacc0104c8ed2eafdc03ebb2014bf0fc62af448158fa220b3d7d4e623b2832be
readonly MAGICPAPER_BUTTER_FONT_SHA256=eb9762a7139a84c96c33e06bccae4af9da3bc5e15b9a20d967d0ca87b684f5b6
readonly MAGICPAPER_851_ARCHIVE_SHA256=d0fc9d139d29905e30eb83a5f2848192cfade5a34e565b3c2d89122150d186dc
readonly MAGICPAPER_851_FONT_SHA256=73cf4b062e3af3711b3b33d230401e215ec6ace00498d8dc645e80f69f000ba9
readonly KOREADER_VERSION=v2026.03
readonly KOREADER_ARCHIVE=/tmp/koreader-remarkable-aarch64-v2026.03.zip
readonly KOREADER_SHA256=56621d5ee66ad94f4f3e2e6d204e8c34be730343f915edc36bb076a043a2e468
KOREADER_VENDOR_RELEASE=$KOREADER_VERSION-$KOREADER_SHA256

if [ "${REMAGIC_SKIP_CHECKS:-0}" != 1 ]; then
    "$ROOT/scripts/check.sh"
    (cd "$MAGICPAPER" && scripts/check.sh)
    (cd "$KOREADER_ADAPTER" && scripts/check.sh)
fi

source_revision() {
    repo=$1
    revision=$(git -C "$repo" rev-parse HEAD)
    if [ -n "$(git -C "$repo" status --porcelain --untracked-files=normal)" ]; then
        printf '%s-dirty\n' "$revision"
    else
        printf '%s\n' "$revision"
    fi
}

BUILD_TMP=$(mktemp -d /tmp/remagic-bundle-build.XXXXXX)
cleanup() {
    rm -rf "$BUILD_TMP"
}
trap cleanup EXIT

stage_magicpaper_font() {
    local archive=$1 archive_sha=$2 font_sha=$3 output=$4
    local actual_archive_sha actual_font_sha staged
    if ! command -v bsdtar >/dev/null 2>&1; then
        echo "bsdtar is required to extract pinned MagicPaper fonts" >&2
        exit 1
    fi
    if [ ! -s "$archive" ]; then
        echo "required MagicPaper font archive is missing: $archive" >&2
        exit 1
    fi
    actual_archive_sha=$(sha256sum "$archive" | awk '{print $1}')
    if [ "$actual_archive_sha" != "$archive_sha" ]; then
        echo "MagicPaper font archive checksum mismatch: $archive" >&2
        exit 1
    fi
    staged="$BUILD_TMP/$(basename "$output")"
    bsdtar -xOf "$archive" '*.ttf' > "$staged"
    actual_font_sha=$(sha256sum "$staged" | awk '{print $1}')
    if [ "$actual_font_sha" != "$font_sha" ]; then
        echo "MagicPaper extracted font checksum mismatch: $archive" >&2
        exit 1
    fi
    install -m 0644 "$staged" "$output"
}

ENV_FILE=$(find "$SDK" -maxdepth 1 -name 'environment-setup-*' -print -quit)
if [ -z "$ENV_FILE" ]; then
    echo "reMarkable SDK environment was not found under $SDK" >&2
    exit 1
fi
unset LD_LIBRARY_PATH
source "$ENV_FILE"

read -r -a STRIP_COMMAND <<<"${STRIP:-}"
if [ "${#STRIP_COMMAND[@]}" -eq 0 ] || ! command -v "${STRIP_COMMAND[0]}" >/dev/null 2>&1; then
    echo "reMarkable SDK STRIP tool is undefined or not executable" >&2
    exit 1
fi

LINKER=$BUILD_TMP/sdk-cc.sh
printf '#!/bin/bash\nexec %s "$@"\n' "$CC" > "$LINKER"
chmod 0755 "$LINKER"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$LINKER"
export QUILL_DIR="$(realpath "$QUILL")"
QUILL_LIBRARY="$QUILL_DIR/build/libquill.so"
if [ ! -s "$QUILL_LIBRARY" ]; then
    echo "Quill runtime library is missing: $QUILL_LIBRARY" >&2
    exit 1
fi

cd "$ROOT"
cargo build --locked --release --target "$TARGET" --workspace
cargo build --locked --release --target "$TARGET" -p remagic-home --features device
(
    cd native/remagic-display-host
    QUILL_LIB_DIR="$QUILL/build" \
        cargo build --locked --release --target "$TARGET" --features device
)

(cd "$MAGICPAPER" && cargo build --locked --release --target "$TARGET")

rm -rf "$ROOT/dist"
mkdir -p "$OUT/bin" "$OUT/lib" "$OUT/fonts" "$OUT/share" \
    "$OUT/opt/magicpaper/fonts" \
    "$OUT/opt/koreader-for-remagic/adapter/releases/.staging/bin" \
    "$OUT/opt/koreader-for-remagic/adapter/releases/.staging/libexec" \
    "$OUT/opt/koreader-for-remagic/adapter/releases/.staging/share/patches" \
    "$OUT/opt/koreader-for-remagic/adapter/releases/.staging/share/fonts" \
    "$OUT/opt/koreader-for-remagic/vendor/releases/$KOREADER_VENDOR_RELEASE" \
    "$OUT/opt/koreader-for-remagic/deployment" "$OUT/shims"
cp target/$TARGET/release/remagicd "$OUT/bin/"
cp target/$TARGET/release/remagic-home "$OUT/bin/"
cp target/$TARGET/release/remagic-runner "$OUT/bin/"
cp target/$TARGET/release/remagicctl "$OUT/bin/"
cp target/$TARGET/release/remagic-vellum-worker "$OUT/bin/"
cp native/remagic-display-host/target/$TARGET/release/remagic-display-host "$OUT/bin/"
cp -R systemd manifests scripts testing "$OUT/"
chmod 0755 \
    "$OUT/scripts/install-device.sh" \
    "$OUT/scripts/magicpaper-remagic" \
    "$OUT/scripts/magicpaper-agent-remagic" \
    "$OUT/scripts/magicpaper-qtfb" \
    "$OUT/scripts/magicpaper-data-migrate" \
    "$OUT/scripts/remagic-recover" \
    "$OUT/scripts/uninstall-device.sh"
KOREADER_ADAPTER_STAGE=$OUT/opt/koreader-for-remagic/adapter/releases/.staging
if [ ! -x "$KOREADER_ADAPTER/scripts/koreader-for-remagic" ]; then
    echo "KOReader adapter wrapper is missing under $KOREADER_ADAPTER" >&2
    exit 1
fi
install -m 0755 "$KOREADER_ADAPTER/scripts/koreader-for-remagic" \
    "$OUT/scripts/koreader-for-remagic"
install -m 0755 "$KOREADER_ADAPTER/scripts/koreader-for-remagic" \
    "$KOREADER_ADAPTER_STAGE/bin/koreader-for-remagic"
for helper in \
    koreader-data-migrate \
    koreader-db-inspect \
    koreader-db-inspect.lua \
    koreader-library-sync \
    koreader-library-index.lua \
    koreader-not-running; do
    if [ ! -f "$KOREADER_ADAPTER/scripts/$helper" ]; then
        echo "KOReader adapter helper is missing: $helper" >&2
        exit 1
    fi
    install -m 0755 "$KOREADER_ADAPTER/scripts/$helper" "$OUT/scripts/$helper"
    install -m 0755 "$KOREADER_ADAPTER/scripts/$helper" \
        "$KOREADER_ADAPTER_STAGE/libexec/$helper"
done
for module in \
    remagic-lifecycle-protocol.lua \
    remagic-open-path.lua; do
    if [ ! -f "$KOREADER_ADAPTER/scripts/$module" ]; then
        echo "KOReader adapter module is missing: $module" >&2
        exit 1
    fi
    install -m 0644 "$KOREADER_ADAPTER/scripts/$module" "$OUT/scripts/$module"
    install -m 0644 "$KOREADER_ADAPTER/scripts/$module" \
        "$KOREADER_ADAPTER_STAGE/libexec/$module"
done
for platform_patch in \
    10-remagic-environment.lua \
    20-remagic-policy.lua \
    21-remagic-lifecycle-v2.lua; do
    install -m 0644 "$KOREADER_ADAPTER/patches/$platform_patch" \
        "$KOREADER_ADAPTER_STAGE/share/patches/$platform_patch"
done
"$KOREADER_ADAPTER/scripts/stage-custom-fonts.sh" \
    "$KOREADER_ADAPTER_STAGE/share/fonts"
cp "$MAGICPAPER/target/$TARGET/release/magicpaper" \
    "$OUT/opt/magicpaper/magicpaper"
stage_magicpaper_font \
    "$MAGICPAPER_851_FONT_ARCHIVE" \
    "$MAGICPAPER_851_ARCHIVE_SHA256" \
    "$MAGICPAPER_851_FONT_SHA256" \
    "$OUT/opt/magicpaper/fonts/851LakeusNightWriting.ttf"
stage_magicpaper_font \
    "$MAGICPAPER_BUTTER_FONT_ARCHIVE" \
    "$MAGICPAPER_BUTTER_ARCHIVE_SHA256" \
    "$MAGICPAPER_BUTTER_FONT_SHA256" \
    "$OUT/opt/magicpaper/fonts/ButterShiSan.ttf"

native/appload-runtime/build-qtfb-shim.sh "$OUT/shims"

VELLUM_VERSION=v0.3.2
VELLUM_BOOTSTRAP=/tmp/vellum-bootstrap-$VELLUM_VERSION.sh
VELLUM_BOOTSTRAP_SHA256=7b0deebc81b28a7d74d95c85e99a4a0a0f6ecaa5b9edb6b858ac61405978ebb9
if [ ! -f "$VELLUM_BOOTSTRAP" ]; then
    curl -fL --retry 3 -o "$VELLUM_BOOTSTRAP" \
        "https://github.com/vellum-dev/vellum-cli/releases/download/$VELLUM_VERSION/bootstrap.sh"
fi
printf '%s  %s\n' "$VELLUM_BOOTSTRAP_SHA256" "$VELLUM_BOOTSTRAP" | sha256sum -c -
cp "$VELLUM_BOOTSTRAP" "$OUT/share/vellum-bootstrap.sh"

if [ ! -f "$KOREADER_ARCHIVE" ]; then
    curl -fL --retry 3 -o "$KOREADER_ARCHIVE" \
        https://github.com/koreader/koreader/releases/download/v2026.03/koreader-remarkable-aarch64-v2026.03.zip
fi
printf '%s  %s\n' "$KOREADER_SHA256" "$KOREADER_ARCHIVE" | sha256sum -c -
KOREADER_VENDOR_PARENT=$OUT/opt/koreader-for-remagic/vendor/releases/$KOREADER_VENDOR_RELEASE
KOREADER_PROGRAM_ROOT=$KOREADER_VENDOR_PARENT/koreader
bsdtar -xf "$KOREADER_ARCHIVE" -C "$KOREADER_VENDOR_PARENT"
grep -qx "$KOREADER_VERSION" "$KOREADER_PROGRAM_ROOT/git-rev" || {
    echo "KOReader archive git-rev does not match v2026.03" >&2
    exit 1
}
if [ -d "$KOREADER_PROGRAM_ROOT/patches" ] && \
   find "$KOREADER_PROGRAM_ROOT/patches" -maxdepth 1 -name '0*-*.lua' \
       -print -quit | grep -q .; then
    echo "KOReader archive has an early-once userpatch; update marker is not safe to consume" >&2
    exit 1
fi
rm -f "$KOREADER_PROGRAM_ROOT/update_once.marker"
[ ! -e "$KOREADER_PROGRAM_ROOT/update_once.marker" ] || {
    echo "KOReader update_once.marker leaked into the managed vendor release" >&2
    exit 1
}
[ -d "$KOREADER_PROGRAM_ROOT/plugins/terminal.koplugin" ] || {
    echo "official KOReader terminal plugin is missing from the vendor release" >&2
    exit 1
}
[ ! -e "$KOREADER_PROGRAM_ROOT/fonts/remagic" ] || {
    echo "ReMagic fonts leaked into the immutable KOReader vendor release" >&2
    exit 1
}
if [ -d "$KOREADER_PROGRAM_ROOT/patches" ] && \
   find "$KOREADER_PROGRAM_ROOT/patches" -maxdepth 1 -name '*remagic*.lua' \
       -print -quit | grep -q .; then
    echo "ReMagic userpatch leaked into the immutable KOReader vendor release" >&2
    exit 1
fi

KOREADER_CJK_FONT=$KOREADER_PROGRAM_ROOT/fonts/noto/NotoSansCJKsc-Regular.otf
if [ ! -f "$KOREADER_CJK_FONT" ] || [ -L "$KOREADER_CJK_FONT" ] || \
   [ ! -s "$KOREADER_CJK_FONT" ]; then
    echo "verified KOReader archive has no safe Noto Sans CJK SC font" >&2
    exit 1
fi
# MagicPaper loads this fixed adjacent filename one glyph at a time whenever a
# selected handwriting face lacks a Simplified Chinese character.
install -m 0644 "$KOREADER_CJK_FONT" \
    "$OUT/opt/magicpaper/fonts/CoverageFallback.ttf"
cmp -s "$KOREADER_CJK_FONT" "$OUT/opt/magicpaper/fonts/CoverageFallback.ttf" || {
    echo "MagicPaper coverage fallback font copy did not verify" >&2
    exit 1
}
MAGICPAPER_UI_FONT=$KOREADER_ADAPTER_STAGE/share/fonts/FZPingXianYaSong.ttf
magicpaper_verify_font_sha256 "$MAGICPAPER_UI_FONT_SHA256" \
    "$MAGICPAPER_UI_FONT" "staged Fangzheng UI font"
install -m 0644 "$MAGICPAPER_UI_FONT" \
    "$OUT/opt/magicpaper/fonts/FZPingXianYaSong.ttf"
magicpaper_verify_font_sha256 "$MAGICPAPER_UI_FONT_SHA256" \
    "$OUT/opt/magicpaper/fonts/FZPingXianYaSong.ttf" \
    "bundled MagicPaper Fangzheng UI font"

KOREADER_ADAPTER_HASH=$(koreader_release_adapter_digest "$KOREADER_ADAPTER_STAGE")
KOREADER_ADAPTER_RELEASE=adapter-$KOREADER_ADAPTER_HASH
KOREADER_ADAPTER_RELEASE_ROOT=$OUT/opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE
mv "$KOREADER_ADAPTER_STAGE" "$KOREADER_ADAPTER_RELEASE_ROOT"
printf 'vendor_release=%s\nadapter_release=%s\n' \
    "$KOREADER_VENDOR_RELEASE" "$KOREADER_ADAPTER_RELEASE" \
    >"$OUT/opt/koreader-for-remagic/deployment/current.env"
(
    cd "$OUT/opt/koreader-for-remagic"
    raw_paths=deployment/.vendor-paths.unsorted
    sorted_paths=deployment/.vendor-paths
    trap 'rm -f "$raw_paths" "$sorted_paths"' EXIT HUP INT TERM
    find "vendor/releases/$KOREADER_VENDOR_RELEASE/koreader" -print \
        >"$raw_paths"
    LC_ALL=C sort "$raw_paths" >"$sorted_paths"
    while IFS= read -r vendor_file; do
        mode=$(stat -c %a "$vendor_file")
        if [ -d "$vendor_file" ] && [ ! -L "$vendor_file" ]; then
            entry_type=d
        elif [ -f "$vendor_file" ] && [ ! -L "$vendor_file" ]; then
            entry_type=f
        else
            echo "unsafe KOReader vendor entry: $vendor_file" >&2
            exit 1
        fi
        printf '%s\t%s\t%s\n' "$entry_type" "$mode" "$vendor_file"
    done <"$sorted_paths" >deployment/vendor.files
    while IFS= read -r vendor_file; do
        [ ! -f "$vendor_file" ] || sha256sum "$vendor_file"
    done <"$sorted_paths" >deployment/vendor.sha256
    rm -f "$raw_paths" "$sorted_paths"
)

sed "s/__REMAGIC_ADAPTER_RELEASE__/$KOREADER_ADAPTER_RELEASE/g" \
    "$KOREADER_ADAPTER/manifests/koreader.toml" >"$OUT/manifests/koreader.toml"
sed \
    -e "s#__REMAGIC_ADAPTER_RELEASE__#$KOREADER_ADAPTER_RELEASE#g" \
    -e "s#__REMAGIC_KOREADER_VENDOR_ROOT__#home/root/apps/koreader-for-remagic/vendor/releases/$KOREADER_VENDOR_RELEASE/koreader#g" \
    -e "s#__REMAGIC_KOREADER_ADAPTER_ROOT__#home/root/apps/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE#g" \
    "$ROOT/testing/manifests/koreader.toml" >"$OUT/testing/manifests/koreader.toml"
koreader_release_load "$OUT/opt/koreader-for-remagic/deployment/current.env"
koreader_release_verify "$OUT/opt/koreader-for-remagic" || {
    echo "KOReader vendor/adapter release integrity check failed" >&2
    exit 1
}

cp "$QUILL_LIBRARY" "$OUT/lib/libquill.so"
"${STRIP_COMMAND[@]}" --strip-unneeded "$OUT/lib/libquill.so"

FONT=${REMAGIC_UI_FONT:-$KOREADER_CJK_FONT}
if [ ! -s "$FONT" ]; then
    echo "required ReMagic UI font is missing: $FONT" >&2
    exit 1
fi
cp "$FONT" "$OUT/fonts/UIFont.ttf"

{
    printf 'built-at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'remagic=%s\n' "$(source_revision "$ROOT")"
    printf 'magicpaper=%s\n' "$(source_revision "$MAGICPAPER")"
    printf 'koreader-adapter=%s\n' "$(source_revision "$KOREADER_ADAPTER")"
    printf 'koreader=2026.03\n'
    printf 'koreader-vendor-release=%s\n' "$KOREADER_VENDOR_RELEASE"
    printf 'koreader-adapter-release=%s\n' "$KOREADER_ADAPTER_RELEASE"
    printf 'qtfb-shim=rm-appload-v0.5.3-standalone\n'
    printf 'display-host-sha256=%s\n' "$(sha256sum "$OUT/bin/remagic-display-host" | awk '{print $1}')"
    printf 'libquill-sha256=%s\n' "$(sha256sum "$OUT/lib/libquill.so" | awk '{print $1}')"
    printf 'magicpaper-ui-font-sha256=%s\n' "$MAGICPAPER_UI_FONT_SHA256"
} > "$OUT/share/build-info.txt"

(
    cd "$OUT"
    find . -type f ! -path './share/bundle.sha256' -print0 \
        | LC_ALL=C sort -z \
        | xargs -0 sha256sum > share/bundle.sha256
)

# The archive is also a deployment contract.  Refuse to publish a bundle whose
# device executables or mandatory runtime library are from another target.
for artifact in \
    "$OUT/bin/remagicd" \
    "$OUT/bin/remagic-home" \
    "$OUT/bin/remagic-runner" \
    "$OUT/bin/remagicctl" \
    "$OUT/bin/remagic-vellum-worker" \
    "$OUT/bin/remagic-display-host" \
    "$OUT/lib/libquill.so" \
    "$OUT/shims/qtfb-shim.so" \
    "$OUT/opt/magicpaper/magicpaper" \
    "$KOREADER_PROGRAM_ROOT/luajit"; do
    if ! file "$artifact" | grep -q 'ARM aarch64'; then
        echo "refusing non-AArch64 bundle artifact: $artifact" >&2
        file "$artifact" >&2
        exit 1
    fi
done
if ! "${READELF:-readelf}" -d "$OUT/bin/remagic-display-host" \
    | grep -q 'Shared library: \[libquill.so\]'; then
    echo "display host does not declare the staged libquill.so runtime contract" >&2
    exit 1
fi

tar --owner=0 --group=0 --numeric-owner -C "$ROOT/dist" \
    -czf "$ROOT/dist/remagic-aarch64.tar.gz" remagic
(cd "$ROOT/dist" && sha256sum remagic-aarch64.tar.gz > remagic-aarch64.tar.gz.sha256)
echo "$ROOT/dist/remagic-aarch64.tar.gz"
