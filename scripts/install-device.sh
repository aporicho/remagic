#!/bin/sh
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "install-device.sh must run as root" >&2
    exit 1
fi

SOURCE_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
APP_ROOT=/home/root/apps/remagic
STATE_ROOT=/home/root/.local/state/remagic/install
MANIFEST_ROOT=/home/root/.local/share/remagic/apps.d
UNIT_ROOT=/usr/lib/systemd/system
ROOT_WAS_READ_ONLY=false

case "$(awk '$2 == "/" { print $4; exit }' /proc/mounts)" in
    ro|ro,*|*,ro|*,ro,*) ROOT_WAS_READ_ONLY=true ;;
esac
if [ "$ROOT_WAS_READ_ONLY" = true ]; then
    mount -o remount,rw /
fi
restore_root_mount() {
    if [ "$ROOT_WAS_READ_ONLY" = true ]; then
        mount -o remount,ro / 2>/dev/null || true
    fi
}
trap restore_root_mount EXIT

# Minimal reMarkable images do not ship coreutils' `install`.  Keep the
# deployment self-contained by providing the small subset we need.
install_file() {
    mode="$1"; source="$2"; target="$3"
    mkdir -p "$(dirname "$target")"
    cp -f "$source" "$target"
    chmod "$mode" "$target"
}

mkdir -p "$APP_ROOT/bin" "$APP_ROOT/lib" "$APP_ROOT/libexec" "$APP_ROOT/fonts" "$APP_ROOT/share"
mkdir -p "$STATE_ROOT" "$MANIFEST_ROOT"

if [ ! -f "$STATE_ROOT/previous.env" ]; then
    old_launcher=disabled
    if systemctl is-enabled riddle-power-launcher.service >/dev/null 2>&1; then
        old_launcher=enabled
    fi
    printf 'RIDDLE_POWER_LAUNCHER=%s\n' "$old_launcher" > "$STATE_ROOT/previous.env"
fi

for binary in remagicd remagic-home remagic-runner remagicctl remagic-vellum-worker; do
    install_file 0755 "$SOURCE_DIR/bin/$binary" "$APP_ROOT/bin/$binary"
done
install_file 0755 "$SOURCE_DIR/scripts/remagic-recover" "$APP_ROOT/libexec/remagic-recover"
install_file 0755 "$SOURCE_DIR/scripts/koreader-remagic" "$APP_ROOT/libexec/koreader-remagic"
install_file 0755 "$SOURCE_DIR/scripts/magicpaper-remagic" "$APP_ROOT/libexec/magicpaper-remagic"
install_file 0755 "$SOURCE_DIR/scripts/magicpaper-agent-remagic" "$APP_ROOT/libexec/magicpaper-agent-remagic"
install_file 0755 "$SOURCE_DIR/scripts/uninstall-device.sh" "$APP_ROOT/libexec/uninstall-device.sh"
install_file 0755 "$SOURCE_DIR/scripts/device-acceptance.sh" "$APP_ROOT/share/device-acceptance.sh"

if [ -f "$SOURCE_DIR/opt/magicpaper/riddle" ]; then
    if [ -f /home/root/apps/riddle/riddle ] && [ ! -f /home/root/apps/riddle/riddle.pre-remagic ]; then
        cp -p /home/root/apps/riddle/riddle /home/root/apps/riddle/riddle.pre-remagic
    fi
    install_file 0755 "$SOURCE_DIR/opt/magicpaper/riddle" /home/root/apps/riddle/riddle
fi

if [ -f "$SOURCE_DIR/lib/libquill.so" ]; then
    install_file 0755 "$SOURCE_DIR/lib/libquill.so" "$APP_ROOT/lib/libquill.so"
fi
if [ -f "$SOURCE_DIR/lib/libremagic-fb.so" ]; then
    install_file 0755 "$SOURCE_DIR/lib/libremagic-fb.so" "$APP_ROOT/lib/libremagic-fb.so"
fi
if [ -f "$SOURCE_DIR/fonts/UIFont.ttf" ]; then
    install_file 0644 "$SOURCE_DIR/fonts/UIFont.ttf" "$APP_ROOT/fonts/UIFont.ttf"
fi
if [ -f "$SOURCE_DIR/share/vellum-bootstrap.sh" ]; then
    install_file 0644 "$SOURCE_DIR/share/vellum-bootstrap.sh" "$APP_ROOT/share/vellum-bootstrap.sh"
fi

if [ ! -x /home/root/apps/koreader/koreader.sh ] && [ -d "$SOURCE_DIR/opt/koreader" ]; then
    rm -rf /home/root/apps/.koreader.new
    cp -a "$SOURCE_DIR/opt/koreader" /home/root/apps/.koreader.new
    old_koreader=/home/root/.paperweight/services/koreader/koreader
    if [ -d "$old_koreader" ]; then
        for state_file in settings.reader.lua settings.reader.lua.old history.lua; do
            if [ -f "$old_koreader/$state_file" ]; then
                cp -p "$old_koreader/$state_file" "/home/root/apps/.koreader.new/$state_file"
            fi
        done
    fi
    mv /home/root/apps/.koreader.new /home/root/apps/koreader
fi

for manifest in "$SOURCE_DIR/manifests/"*.toml; do install_file 0644 "$manifest" "$MANIFEST_ROOT/$(basename "$manifest")"; done
install_file 0644 "$SOURCE_DIR/systemd/remagicd.service" "$UNIT_ROOT/remagicd.service"
install_file 0644 "$SOURCE_DIR/systemd/remagic-home.service" "$UNIT_ROOT/remagic-home.service"
install_file 0644 "$SOURCE_DIR/systemd/remagic-app@.service" "$UNIT_ROOT/remagic-app@.service"
install_file 0644 "$SOURCE_DIR/systemd/remagic-recover.service" "$UNIT_ROOT/remagic-recover.service"
install_file 0644 "$SOURCE_DIR/systemd/magicpaper-agent.service" "$UNIT_ROOT/magicpaper-agent.service"
mkdir -p "$UNIT_ROOT/multi-user.target.wants"
ln -sf ../remagicd.service "$UNIT_ROOT/multi-user.target.wants/remagicd.service"
ln -sf ../magicpaper-agent.service "$UNIT_ROOT/multi-user.target.wants/magicpaper-agent.service"

restore_root_mount
ROOT_WAS_READ_ONLY=false

systemctl stop riddle-power-launcher.service riddle-takeover.service 2>/dev/null || true
systemctl disable riddle-power-launcher.service 2>/dev/null || true
"$APP_ROOT/libexec/remagic-recover"
systemctl daemon-reload
systemctl restart remagicd.service
systemctl start magicpaper-agent.service

echo "Remagic Manager installed. The original interface remains the boot default."
echo "Triple-press power to open the manager."
