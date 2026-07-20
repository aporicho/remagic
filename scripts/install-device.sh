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
WANTS_ROOT=/etc/systemd/system/multi-user.target.wants
ROOT_WAS_READ_ONLY=false
SERVICES_STOPPED=false
INSTALL_COMMITTED=false

for required in \
    "$SOURCE_DIR/bin/remagicd" \
    "$SOURCE_DIR/bin/remagicctl" \
    "$SOURCE_DIR/bin/remagic-home" \
    "$SOURCE_DIR/bin/remagic-runner" \
    "$SOURCE_DIR/bin/remagic-vellum-worker" \
    "$SOURCE_DIR/bin/remagic-uinput-tap" \
    "$SOURCE_DIR/runtime/remagic-appload-runtime" \
    "$SOURCE_DIR/runtime/_start.qml" \
    "$SOURCE_DIR/runtime/LICENSE.appload" \
    "$SOURCE_DIR/share/build-info.txt" \
    "$SOURCE_DIR/share/bundle.sha256" \
    "$SOURCE_DIR/shims/qtfb-shim.so" \
    "$SOURCE_DIR/opt/magicpaper/riddle-qtfb" \
    "$SOURCE_DIR/opt/magicpaper/fonts/851LakeusNightWriting.ttf" \
    "$SOURCE_DIR/opt/magicpaper/fonts/ButterShiSan.ttf" \
    "$SOURCE_DIR/runtime/applications_root/magicpaper/external.manifest.json" \
    "$SOURCE_DIR/runtime/applications_root/magicpaper/icon.png" \
    "$SOURCE_DIR/runtime/applications_root/koreader/external.manifest.json" \
    "$SOURCE_DIR/runtime/applications_root/koreader/icon.png" \
    "$SOURCE_DIR/opt/koreader/fonts/remagic/STDongGuanTi-Regular.ttf" \
    "$SOURCE_DIR/opt/koreader/fonts/remagic/STDongGuanTi-Bold.ttf" \
    "$SOURCE_DIR/opt/koreader/fonts/remagic/STDongGuanTi-Light.ttf" \
    "$SOURCE_DIR/opt/koreader/fonts/remagic/FZPingXianYaSong.ttf" \
    "$SOURCE_DIR/opt/koreader/patches/2-remagic-runtime.lua" \
    "$SOURCE_DIR/scripts/remagic-recover" \
    "$SOURCE_DIR/scripts/remagic-runtime" \
    "$SOURCE_DIR/scripts/koreader-remagic" \
    "$SOURCE_DIR/scripts/koreader-data-migrate" \
    "$SOURCE_DIR/scripts/koreader-db-inspect" \
    "$SOURCE_DIR/scripts/koreader-db-inspect.lua" \
    "$SOURCE_DIR/scripts/koreader-library-sync" \
    "$SOURCE_DIR/scripts/koreader-library-index.lua" \
    "$SOURCE_DIR/scripts/magicpaper-remagic" \
    "$SOURCE_DIR/scripts/magicpaper-agent-remagic" \
    "$SOURCE_DIR/scripts/magicpaper-qtfb" \
    "$SOURCE_DIR/scripts/riddle-env" \
    "$SOURCE_DIR/scripts/uninstall-device.sh" \
    "$SOURCE_DIR/scripts/device-acceptance.sh" \
    "$SOURCE_DIR/manifests/magicpaper.toml" \
    "$SOURCE_DIR/manifests/koreader.toml" \
    "$SOURCE_DIR/systemd/remagicd.service" \
    "$SOURCE_DIR/systemd/remagic-home.service" \
    "$SOURCE_DIR/systemd/remagic-runtime.service" \
    "$SOURCE_DIR/systemd/remagic-app@.service" \
    "$SOURCE_DIR/systemd/remagic-recover.service" \
    "$SOURCE_DIR/systemd/magicpaper-agent.service"; do
    if [ ! -s "$required" ]; then
        echo "install-device.sh: incomplete bundle, missing or empty $required" >&2
        exit 1
    fi
done

if ! (cd "$SOURCE_DIR" && sha256sum -c share/bundle.sha256 >/dev/null); then
    echo "install-device.sh: bundle content checksum verification failed" >&2
    exit 1
fi

for executable in \
    "$SOURCE_DIR/bin/remagicd" \
    "$SOURCE_DIR/bin/remagicctl" \
    "$SOURCE_DIR/runtime/remagic-appload-runtime" \
    "$SOURCE_DIR/scripts/remagic-runtime" \
    "$SOURCE_DIR/scripts/koreader-remagic" \
    "$SOURCE_DIR/scripts/koreader-library-sync" \
    "$SOURCE_DIR/scripts/magicpaper-qtfb"; do
    if [ ! -x "$executable" ]; then
        echo "install-device.sh: required executable is not executable: $executable" >&2
        exit 1
    fi
done

if [ -d "$SOURCE_DIR/opt/koreader" ] && [ ! -x "$SOURCE_DIR/opt/koreader/reader.lua" ]; then
    echo "install-device.sh: bundled KOReader is incomplete (reader.lua missing)" >&2
    exit 1
fi

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
finish_install() {
    status=$?
    if [ "$status" -ne 0 ] && [ "$SERVICES_STOPPED" = true ] && [ "$INSTALL_COMMITTED" = false ]; then
        echo "install-device.sh: installation failed; restoring the stock interface" >&2
        systemctl stop magicpaper-agent.service remagic-runtime.service \
            'remagic-app@*.service' remagic-home.service remagicd.service 2>/dev/null || true
        rm -f /tmp/qtfb.sock /run/remagic/runtime-app.sock /tmp/epframebuffer.lock
        systemctl unmask --runtime xochitl.service 2>/dev/null || true
        systemctl reset-failed xochitl.service paperweight.service 2>/dev/null || true
        systemctl start xochitl.service 2>/dev/null || true
        if [ "$(systemctl show --property=LoadState --value paperweight.service 2>/dev/null || true)" = loaded ]; then
            systemctl start paperweight.service 2>/dev/null || true
        fi
    fi
    restore_root_mount
    trap - EXIT
    exit "$status"
}
trap finish_install EXIT

wait_active() {
    unit=$1
    attempts=0
    while [ "$attempts" -lt 60 ]; do
        [ "$(systemctl is-active "$unit" 2>/dev/null || true)" = active ] && return 0
        sleep 0.1
        attempts=$((attempts + 1))
    done
    return 1
}

# Return display ownership before replacing any live executable.  If an older
# manager is healthy, let it perform its serialized transition first.
if [ -x "$APP_ROOT/bin/remagicctl" ]; then
    "$APP_ROOT/bin/remagicctl" system >/dev/null 2>&1 || true
    wait_active xochitl.service || true
fi
systemctl stop remagic-runtime.service 'remagic-app@*.service' remagic-home.service \
    magicpaper-agent.service remagicd.service riddle-power-launcher.service 2>/dev/null || true
SERVICES_STOPPED=true
# At this point every managed runtime has been stopped, so any remaining QTFB
# shared-memory object is stale and must not leak into the newly installed run.
rm -f /dev/shm/qtfb_*

# Minimal reMarkable images do not ship coreutils' `install`.  Keep the
# deployment self-contained by providing the small subset we need.
install_file() {
    mode="$1"; source="$2"; target="$3"
    mkdir -p "$(dirname "$target")"
    cp -f "$source" "$target"
    chmod "$mode" "$target"
}

mkdir -p "$APP_ROOT/bin" "$APP_ROOT/lib" "$APP_ROOT/libexec" "$APP_ROOT/fonts" "$APP_ROOT/share" "$APP_ROOT/shims"
mkdir -p "$APP_ROOT/runtime/applications_root/magicpaper" "$APP_ROOT/runtime/applications_root/koreader" "$APP_ROOT/opt/magicpaper/fonts"
mkdir -p /home/root/apps/remagic-koreader/bin
mkdir -p /home/root/apps/remagic-koreader/libexec
mkdir -p "$STATE_ROOT" "$MANIFEST_ROOT"

old_launcher=disabled
if [ -L /usr/lib/systemd/system/multi-user.target.wants/riddle-power-launcher.service ] || \
   [ -L /etc/systemd/system/multi-user.target.wants/riddle-power-launcher.service ]; then
    old_launcher=enabled
fi
# Correct an older install's inaccurate `is-enabled` snapshot when the vendor
# wants link is still visibly present; otherwise retain the first snapshot.
if [ ! -f "$STATE_ROOT/previous.env" ] || [ "$old_launcher" = enabled ]; then
    printf 'RIDDLE_POWER_LAUNCHER=%s\n' "$old_launcher" > "$STATE_ROOT/previous.env"
fi
rm -f /usr/lib/systemd/system/multi-user.target.wants/riddle-power-launcher.service
rm -f /etc/systemd/system/multi-user.target.wants/riddle-power-launcher.service

for binary in remagicd remagic-home remagic-runner remagicctl remagic-vellum-worker remagic-uinput-tap; do
    install_file 0755 "$SOURCE_DIR/bin/$binary" "$APP_ROOT/bin/$binary"
done
install_file 0755 "$SOURCE_DIR/scripts/remagic-recover" "$APP_ROOT/libexec/remagic-recover"
install_file 0755 "$SOURCE_DIR/scripts/koreader-remagic" "$APP_ROOT/libexec/koreader-remagic"
install_file 0755 "$SOURCE_DIR/scripts/koreader-remagic" /home/root/apps/remagic-koreader/bin/koreader-remagic
install_file 0755 "$SOURCE_DIR/scripts/koreader-data-migrate" /home/root/apps/remagic-koreader/libexec/koreader-data-migrate
install_file 0755 "$SOURCE_DIR/scripts/koreader-db-inspect" /home/root/apps/remagic-koreader/libexec/koreader-db-inspect
install_file 0644 "$SOURCE_DIR/scripts/koreader-db-inspect.lua" /home/root/apps/remagic-koreader/libexec/koreader-db-inspect.lua
install_file 0755 "$SOURCE_DIR/scripts/koreader-library-sync" /home/root/apps/remagic-koreader/libexec/koreader-library-sync
install_file 0644 "$SOURCE_DIR/scripts/koreader-library-index.lua" /home/root/apps/remagic-koreader/libexec/koreader-library-index.lua
install_file 0755 "$SOURCE_DIR/scripts/magicpaper-remagic" "$APP_ROOT/libexec/magicpaper-remagic"
install_file 0755 "$SOURCE_DIR/scripts/magicpaper-agent-remagic" "$APP_ROOT/libexec/magicpaper-agent-remagic"
install_file 0755 "$SOURCE_DIR/scripts/magicpaper-qtfb" "$APP_ROOT/libexec/magicpaper-qtfb"
install_file 0644 "$SOURCE_DIR/scripts/riddle-env" "$APP_ROOT/libexec/riddle-env"
install_file 0755 "$SOURCE_DIR/scripts/remagic-runtime" "$APP_ROOT/libexec/remagic-runtime"
install_file 0755 "$SOURCE_DIR/scripts/uninstall-device.sh" "$APP_ROOT/libexec/uninstall-device.sh"
install_file 0755 "$SOURCE_DIR/scripts/device-acceptance.sh" "$APP_ROOT/share/device-acceptance.sh"

if [ -f "$SOURCE_DIR/opt/magicpaper/riddle" ]; then
    if [ -f /home/root/apps/riddle/riddle ] && [ ! -f /home/root/apps/riddle/riddle.pre-remagic ]; then
        cp -p /home/root/apps/riddle/riddle /home/root/apps/riddle/riddle.pre-remagic
    fi
    install_file 0755 "$SOURCE_DIR/opt/magicpaper/riddle" /home/root/apps/riddle/riddle
fi
if [ -f "$SOURCE_DIR/opt/magicpaper/riddle-qtfb" ]; then
    install_file 0755 "$SOURCE_DIR/opt/magicpaper/riddle-qtfb" "$APP_ROOT/opt/magicpaper/riddle-qtfb"
fi
for font in "$SOURCE_DIR/opt/magicpaper/fonts/"*.ttf; do
    if [ -f "$font" ]; then
        install_file 0644 "$font" "$APP_ROOT/opt/magicpaper/fonts/$(basename "$font")"
    fi
done

install_file 0755 "$SOURCE_DIR/runtime/remagic-appload-runtime" "$APP_ROOT/runtime/remagic-appload-runtime"
install_file 0644 "$SOURCE_DIR/runtime/_start.qml" "$APP_ROOT/runtime/_start.qml"
install_file 0644 "$SOURCE_DIR/runtime/LICENSE.appload" "$APP_ROOT/runtime/LICENSE.appload"
for app_id in magicpaper koreader; do
    install_file 0644 "$SOURCE_DIR/runtime/applications_root/$app_id/external.manifest.json" \
        "$APP_ROOT/runtime/applications_root/$app_id/external.manifest.json"
    install_file 0644 "$SOURCE_DIR/runtime/applications_root/$app_id/icon.png" \
        "$APP_ROOT/runtime/applications_root/$app_id/icon.png"
done
install_file 0755 "$SOURCE_DIR/shims/qtfb-shim.so" "$APP_ROOT/shims/qtfb-shim.so"

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
install_file 0644 "$SOURCE_DIR/share/build-info.txt" "$APP_ROOT/share/build-info.txt"

if [ ! -x /home/root/apps/koreader/reader.lua ] && [ -d "$SOURCE_DIR/opt/koreader" ]; then
    rm -rf /home/root/apps/.koreader.new
    cp -a "$SOURCE_DIR/opt/koreader" /home/root/apps/.koreader.new
    if [ ! -x /home/root/apps/.koreader.new/reader.lua ]; then
        echo "install-device.sh: staged KOReader has no executable reader.lua" >&2
        exit 1
    fi
    old_koreader=/home/root/.paperweight/services/koreader/koreader
    if [ -d "$old_koreader" ]; then
        for state_file in settings.reader.lua settings.reader.lua.old history.lua; do
            if [ -f "$old_koreader/$state_file" ]; then
                cp -p "$old_koreader/$state_file" "/home/root/apps/.koreader.new/$state_file"
            fi
        done
    fi
    broken_koreader=/home/root/apps/koreader.pre-remagic-broken
    if [ -e /home/root/apps/koreader ]; then
        rm -rf "$broken_koreader"
        mv /home/root/apps/koreader "$broken_koreader"
    fi
    if ! mv /home/root/apps/.koreader.new /home/root/apps/koreader; then
        if [ -e "$broken_koreader" ]; then
            mv "$broken_koreader" /home/root/apps/koreader
        fi
        exit 1
    fi
fi

# Keep any existing KOReader preference file.  Only seed Simplified Chinese on
# a genuinely fresh data directory; KOReader will merge its remaining defaults
# on first launch.
if [ ! -e /home/root/apps/koreader/settings.reader.lua ] && \
   [ ! -e /home/root/apps/koreader/settings.reader.lua.old ]; then
    printf '%s\n' \
        '-- /home/root/apps/koreader/settings.reader.lua' \
        'return {' \
        '    ["language"] = "zh_CN",' \
        '}' > /home/root/apps/koreader/settings.reader.lua
    chmod 0644 /home/root/apps/koreader/settings.reader.lua
fi

# Custom reading fonts are managed independently from the KOReader program
# directory.  Always refresh them during upgrades, including when an existing
# KOReader installation and all of its settings are preserved.
for font in \
    STDongGuanTi-Regular.ttf \
    STDongGuanTi-Bold.ttf \
    STDongGuanTi-Light.ttf \
    FZPingXianYaSong.ttf; do
    install_file 0644 "$SOURCE_DIR/opt/koreader/fonts/remagic/$font" \
        "/home/root/apps/koreader/fonts/remagic/$font"
done

# This userpatch is lifecycle glue, not user preference data. Refresh it on
# every manager upgrade even when the existing KOReader installation is kept.
mkdir -p /home/root/apps/koreader/patches
install_file 0644 "$SOURCE_DIR/opt/koreader/patches/2-remagic-runtime.lua" \
    /home/root/apps/koreader/patches/2-remagic-runtime.lua

# KOReader itself is now stopped, so its SQLite state can be validated and
# repaired atomically before the first managed launch.
/home/root/apps/remagic-koreader/libexec/koreader-data-migrate

for manifest in "$SOURCE_DIR/manifests/"*.toml; do install_file 0644 "$manifest" "$MANIFEST_ROOT/$(basename "$manifest")"; done
install_file 0644 "$SOURCE_DIR/systemd/remagicd.service" "$UNIT_ROOT/remagicd.service"
install_file 0644 "$SOURCE_DIR/systemd/remagic-home.service" "$UNIT_ROOT/remagic-home.service"
install_file 0644 "$SOURCE_DIR/systemd/remagic-runtime.service" "$UNIT_ROOT/remagic-runtime.service"
install_file 0644 "$SOURCE_DIR/systemd/remagic-app@.service" "$UNIT_ROOT/remagic-app@.service"
install_file 0644 "$SOURCE_DIR/systemd/remagic-recover.service" "$UNIT_ROOT/remagic-recover.service"
install_file 0644 "$SOURCE_DIR/systemd/magicpaper-agent.service" "$UNIT_ROOT/magicpaper-agent.service"
rm -f "$UNIT_ROOT/multi-user.target.wants/remagicd.service"
rm -f "$UNIT_ROOT/multi-user.target.wants/magicpaper-agent.service"
mkdir -p "$WANTS_ROOT"
ln -sf /usr/lib/systemd/system/remagicd.service "$WANTS_ROOT/remagicd.service"
ln -sf /usr/lib/systemd/system/magicpaper-agent.service "$WANTS_ROOT/magicpaper-agent.service"

restore_root_mount
ROOT_WAS_READ_ONLY=false

"$APP_ROOT/libexec/remagic-recover"
systemctl daemon-reload
systemctl restart remagicd.service
wait_active remagicd.service
"$APP_ROOT/bin/remagicctl" status >/dev/null
systemctl start magicpaper-agent.service
wait_active magicpaper-agent.service
INSTALL_COMMITTED=true

echo "Remagic Manager installed. The original interface remains the boot default."
echo "Triple-press power to open the manager."
