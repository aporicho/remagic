#!/bin/bash
set -euo pipefail

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TARGET=aarch64-unknown-linux-gnu
OUT="$ROOT/dist/remagic-manager"
SDK=${RM_SDK:-/home/aporicho/rm-sdk-chiappa-3.27}
QUILL=${QUILL_DIR:-$ROOT/../quill-move}
RIDDLE=${MAGICPAPER_DIR:-$ROOT/../riddle-move}

ENV_FILE=$(find "$SDK" -maxdepth 1 -name 'environment-setup-*' -print -quit)
if [ -z "$ENV_FILE" ]; then
    echo "reMarkable SDK environment was not found under $SDK" >&2
    exit 1
fi
unset LD_LIBRARY_PATH
source "$ENV_FILE"

LINKER=/tmp/remagic-sdk-cc.sh
printf '#!/bin/bash\nexec %s "$@"\n' "$CC" > "$LINKER"
chmod 0755 "$LINKER"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="$LINKER"
export QUILL_DIR="$(realpath "$QUILL")"

cd "$ROOT"
cargo build --release --target "$TARGET" --workspace
cargo build --release --target "$TARGET" -p remagic-home --features device

(cd "$RIDDLE" && RM_SDK="$SDK" QUILL_DIR="$QUILL" ./build-takeover.sh)

$CC -fPIC -shared -O2 -std=gnu11 \
    -I "$QUILL/src" native/koreader-quill-bridge.c \
    -L "$QUILL/build" -L "$QUILL/vendor" \
    -lquill -lqsgepaper -lQt6Gui -lQt6Core -lstdc++ -ldl -lpthread \
    -Wl,-rpath,/home/root/apps/remagic/lib:/usr/lib/plugins/scenegraph \
    -o /tmp/libremagic-fb.so

rm -rf "$ROOT/dist"
mkdir -p "$OUT/bin" "$OUT/lib" "$OUT/fonts" "$OUT/share" "$OUT/opt/magicpaper"
cp target/$TARGET/release/remagicd "$OUT/bin/"
cp target/$TARGET/release/remagic-home "$OUT/bin/"
cp target/$TARGET/release/remagic-runner "$OUT/bin/"
cp target/$TARGET/release/remagicctl "$OUT/bin/"
cp target/$TARGET/release/remagic-vellum-worker "$OUT/bin/"
cp -R systemd manifests scripts "$OUT/"
cp "$RIDDLE/target/$TARGET/release/riddle-takeover" "$OUT/opt/magicpaper/riddle"

VELLUM_BOOTSTRAP=/tmp/vellum-bootstrap.sh
VELLUM_BOOTSTRAP_SHA256=7b0deebc81b28a7d74d95c85e99a4a0a0f6ecaa5b9edb6b858ac61405978ebb9
if [ ! -f "$VELLUM_BOOTSTRAP" ]; then
    curl -fL --retry 3 -o "$VELLUM_BOOTSTRAP" \
        https://github.com/vellum-dev/vellum-cli/releases/latest/download/bootstrap.sh
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

if [ -f "$ROOT/../quill-move/build/libquill.so" ]; then
    cp "$ROOT/../quill-move/build/libquill.so" "$OUT/lib/"
fi
cp /tmp/libremagic-fb.so "$OUT/lib/"

FONT=${REMAGIC_UI_FONT:-/home/aporicho/Desktop/github/M5StopWatch-UserDemo/components/lvgl/scripts/built_in_font/SourceHanSansSC-Normal.otf}
if [ -f "$FONT" ]; then
    cp "$FONT" "$OUT/fonts/UIFont.ttf"
fi

tar -C "$ROOT/dist" -czf "$ROOT/dist/remagic-manager-aarch64.tar.gz" remagic-manager
echo "$ROOT/dist/remagic-manager-aarch64.tar.gz"
