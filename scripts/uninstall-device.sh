#!/bin/sh
set -eu

if [ "$(id -u)" -ne 0 ]; then
    echo "uninstall-device.sh must run as root" >&2
    exit 1
fi

APP_ROOT=/home/root/apps/remagic
STATE_ROOT=/home/root/.local/state/remagic/install
PURGE=${1:-}
UNIT_ROOT=/usr/lib/systemd/system
WANTS_ROOT=/etc/systemd/system/multi-user.target.wants
MANIFEST_ROOT=/home/root/.local/share/remagic/apps.d

if [ -x "$APP_ROOT/bin/remagicctl" ]; then
    "$APP_ROOT/bin/remagicctl" system >/dev/null 2>&1 || true
fi
systemctl stop remagic-runtime.service remagic-home.service 'remagic-app@*.service' \
    magicpaper-agent.service remagicd.service 2>/dev/null || true
if [ -x "$APP_ROOT/libexec/remagic-recover" ]; then
    "$APP_ROOT/libexec/remagic-recover"
fi

root_was_read_only=false
case "$(awk '$2 == "/" { print $4; exit }' /proc/mounts)" in
    ro|ro,*|*,ro|*,ro,*) root_was_read_only=true ;;
esac
if [ "$root_was_read_only" = true ]; then
    mount -o remount,rw /
fi
rm -f "$UNIT_ROOT/multi-user.target.wants/remagicd.service"
rm -f "$UNIT_ROOT/multi-user.target.wants/magicpaper-agent.service"
rm -f "$WANTS_ROOT/remagicd.service"
rm -f "$WANTS_ROOT/magicpaper-agent.service"
rm -f "$UNIT_ROOT/remagicd.service"
rm -f "$UNIT_ROOT/remagic-home.service"
rm -f "$UNIT_ROOT/remagic-runtime.service"
rm -f "$UNIT_ROOT/remagic-app@.service"
rm -f "$UNIT_ROOT/remagic-recover.service"
rm -f "$UNIT_ROOT/magicpaper-agent.service"

if [ -f "$STATE_ROOT/previous.env" ]; then
    . "$STATE_ROOT/previous.env"
    if [ "${RIDDLE_POWER_LAUNCHER:-disabled}" = enabled ]; then
        mkdir -p "$WANTS_ROOT"
        ln -sf /usr/lib/systemd/system/riddle-power-launcher.service \
            "$WANTS_ROOT/riddle-power-launcher.service"
    fi
fi

if [ "$root_was_read_only" = true ]; then
    mount -o remount,ro /
fi
systemctl daemon-reload
if [ "${RIDDLE_POWER_LAUNCHER:-disabled}" = enabled ]; then
    systemctl start riddle-power-launcher.service 2>/dev/null || true
fi

rm -rf "$APP_ROOT" /home/root/apps/remagic-koreader
rm -f /tmp/qtfb.sock /tmp/qtfb-* \
    /run/remagic/foreground-app /run/magicpaper-koreader.request
rm -f "$MANIFEST_ROOT/magicpaper.toml" "$MANIFEST_ROOT/koreader.toml"
if [ -f /home/root/apps/riddle/riddle.pre-remagic ]; then
    mv /home/root/apps/riddle/riddle.pre-remagic /home/root/apps/riddle/riddle
fi
if [ "$PURGE" = "--purge" ]; then
    rm -rf /home/root/.local/state/remagic /home/root/.local/share/remagic
fi

echo "Remagic Manager removed; the original interface has been restored."
