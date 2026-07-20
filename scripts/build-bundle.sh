#!/bin/bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TARGET=aarch64-unknown-linux-gnu
OUT="$ROOT/dist/remagic-manager"
SDK=${RM_SDK:-/home/aporicho/rm-sdk-chiappa-3.27}
QUILL=${QUILL_DIR:-$ROOT/../quill-move}
RIDDLE=${MAGICPAPER_DIR:-$ROOT/../riddle-move}
KOREADER_ADAPTER=${KOREADER_ADAPTER_DIR:-$ROOT/../remagic-koreader}

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

ENV_FILE=$(find "$SDK" -maxdepth 1 -name 'environment-setup-*' -print -quit)
if [ -z "$ENV_FILE" ]; then
    echo "reMarkable SDK environment was not found under $SDK" >&2
    exit 1
fi
unset LD_LIBRARY_PATH
source "$ENV_FILE"

LINKER=$BUILD_TMP/sdk-cc.sh
printf '#!/bin/bash\nexec %s "$@"\n' "$CC" > "$LINKER"
chmod 0755 "$LINKER"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$LINKER"
export QUILL_DIR="$(realpath "$QUILL")"

cd "$ROOT"
cargo build --locked --release --target "$TARGET" --workspace
cargo build --locked --release --target "$TARGET" -p remagic-home --features device

(cd "$RIDDLE" && RM_SDK="$SDK" QUILL_DIR="$QUILL" ./build-takeover.sh)
(cd "$RIDDLE" && cargo build --locked --release --target "$TARGET")

$CC -O2 -Wall -Wextra -std=c11 native/remagic-uinput-tap.c \
    -o "$BUILD_TMP/remagic-uinput-tap"

$CC -fPIC -shared -O2 -std=gnu11 \
    -I "$QUILL/src" native/koreader-quill-bridge.c \
    -L "$QUILL/build" -L "$QUILL/vendor" \
    -lquill -lqsgepaper -lQt6Gui -lQt6Core -lstdc++ -ldl -lpthread \
    -Wl,-rpath,/home/root/apps/remagic/lib:/usr/lib/plugins/scenegraph \
    -o "$BUILD_TMP/libremagic-fb.so"

rm -rf "$ROOT/dist"
mkdir -p "$OUT/bin" "$OUT/lib" "$OUT/fonts" "$OUT/share" "$OUT/opt/magicpaper/fonts" "$OUT/shims"
cp target/$TARGET/release/remagicd "$OUT/bin/"
cp target/$TARGET/release/remagic-home "$OUT/bin/"
cp target/$TARGET/release/remagic-runner "$OUT/bin/"
cp target/$TARGET/release/remagicctl "$OUT/bin/"
cp target/$TARGET/release/remagic-vellum-worker "$OUT/bin/"
cp "$BUILD_TMP/remagic-uinput-tap" "$OUT/bin/"
cp -R systemd manifests scripts "$OUT/"
if [ ! -x "$KOREADER_ADAPTER/scripts/koreader-remagic" ]; then
    echo "KOReader adapter wrapper is missing under $KOREADER_ADAPTER" >&2
    exit 1
fi
cp "$KOREADER_ADAPTER/scripts/koreader-remagic" "$OUT/scripts/koreader-remagic"
for helper in \
    koreader-data-migrate \
    koreader-db-inspect \
    koreader-db-inspect.lua \
    koreader-library-sync \
    koreader-library-index.lua; do
    if [ ! -f "$KOREADER_ADAPTER/scripts/$helper" ]; then
        echo "KOReader adapter helper is missing: $helper" >&2
        exit 1
    fi
    cp "$KOREADER_ADAPTER/scripts/$helper" "$OUT/scripts/$helper"
done
cp "$KOREADER_ADAPTER/manifests/koreader.toml" "$OUT/manifests/koreader.toml"
cp "$RIDDLE/target/$TARGET/release/riddle-takeover" "$OUT/opt/magicpaper/riddle"
cp "$RIDDLE/target/$TARGET/release/riddle" "$OUT/opt/magicpaper/riddle-qtfb"
if [ -d "$RIDDLE/dist/riddle/fonts" ]; then
    cp "$RIDDLE/dist/riddle/fonts/"*.ttf "$OUT/opt/magicpaper/fonts/"
fi
for required_font in 851LakeusNightWriting.ttf ButterShiSan.ttf; do
    if [ ! -s "$OUT/opt/magicpaper/fonts/$required_font" ]; then
        echo "required MagicPaper font is missing: $required_font" >&2
        exit 1
    fi
done

native/appload-runtime/build-runtime.sh "$OUT/runtime"
cp "$OUT/runtime/shims/qtfb-shim.so" "$OUT/shims/qtfb-shim.so"
rm -rf "$OUT/runtime/shims"
mkdir -p "$OUT/runtime/applications_root/magicpaper" "$OUT/runtime/applications_root/koreader"
cp native/appload-runtime/apps/magicpaper/external.manifest.json \
    "$OUT/runtime/applications_root/magicpaper/"
cp native/appload-runtime/apps/koreader/external.manifest.json \
    "$OUT/runtime/applications_root/koreader/"
cp "$RIDDLE/icon.png" "$OUT/runtime/applications_root/magicpaper/icon.png"

VELLUM_VERSION=v0.3.2
VELLUM_BOOTSTRAP=/tmp/vellum-bootstrap-$VELLUM_VERSION.sh
VELLUM_BOOTSTRAP_SHA256=7b0deebc81b28a7d74d95c85e99a4a0a0f6ecaa5b9edb6b858ac61405978ebb9
if [ ! -f "$VELLUM_BOOTSTRAP" ]; then
    curl -fL --retry 3 -o "$VELLUM_BOOTSTRAP" \
        "https://github.com/vellum-dev/vellum-cli/releases/download/$VELLUM_VERSION/bootstrap.sh"
fi
printf '%s  %s\n' "$VELLUM_BOOTSTRAP_SHA256" "$VELLUM_BOOTSTRAP" | sha256sum -c -
cp "$VELLUM_BOOTSTRAP" "$OUT/share/vellum-bootstrap.sh"

KOREADER_ARCHIVE=/tmp/koreader-remarkable-aarch64-v2026.03.zip
KOREADER_SHA256=56621d5ee66ad94f4f3e2e6d204e8c34be730343f915edc36bb076a043a2e468
if [ ! -f "$KOREADER_ARCHIVE" ]; then
    curl -fL --retry 3 -o "$KOREADER_ARCHIVE" \
        https://github.com/koreader/koreader/releases/download/v2026.03/koreader-remarkable-aarch64-v2026.03.zip
fi
printf '%s  %s\n' "$KOREADER_SHA256" "$KOREADER_ARCHIVE" | sha256sum -c -
bsdtar -xf "$KOREADER_ARCHIVE" -C "$OUT/opt"
mkdir -p "$OUT/opt/koreader/patches"
cp "$KOREADER_ADAPTER/patches/2-remagic-runtime.lua" \
    "$OUT/opt/koreader/patches/2-remagic-runtime.lua"
"$KOREADER_ADAPTER/scripts/stage-custom-fonts.sh" \
    "$OUT/opt/koreader/fonts/remagic"
cp "$OUT/opt/koreader/icon.png" "$OUT/runtime/applications_root/koreader/icon.png"

if [ -f "$ROOT/../quill-move/build/libquill.so" ]; then
    cp "$ROOT/../quill-move/build/libquill.so" "$OUT/lib/"
fi
cp "$BUILD_TMP/libremagic-fb.so" "$OUT/lib/"

FONT=${REMAGIC_UI_FONT:-$OUT/opt/koreader/fonts/noto/NotoSansCJKsc-Regular.otf}
if [ ! -s "$FONT" ]; then
    echo "required Remagic UI font is missing: $FONT" >&2
    exit 1
fi
cp "$FONT" "$OUT/fonts/UIFont.ttf"

{
    printf 'built-at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'remagic-manager=%s\n' "$(source_revision "$ROOT")"
    printf 'magicpaper=%s\n' "$(source_revision "$RIDDLE")"
    printf 'koreader-adapter=%s\n' "$(source_revision "$KOREADER_ADAPTER")"
    printf 'koreader=2026.03\n'
    printf 'appload=0.5.3-remagic\n'
} > "$OUT/share/build-info.txt"

(
    cd "$OUT"
    find . -type f ! -path './share/bundle.sha256' -print \
        | LC_ALL=C sort \
        | xargs sha256sum > share/bundle.sha256
)

tar -C "$ROOT/dist" -czf "$ROOT/dist/remagic-manager-aarch64.tar.gz" remagic-manager
(cd "$ROOT/dist" && sha256sum remagic-manager-aarch64.tar.gz > remagic-manager-aarch64.tar.gz.sha256)
echo "$ROOT/dist/remagic-manager-aarch64.tar.gz"
