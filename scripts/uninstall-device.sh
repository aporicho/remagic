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

if [ -x "$APP_ROOT/libexec/remagic-recover" ]; then
    "$APP_ROOT/libexec/remagic-recover"
fi
systemctl stop remagicd.service 2>/dev/null || true
systemctl stop magicpaper-agent.service 2>/dev/null || true
systemctl stop remagic-home.service 'remagic-app@*.service' 2>/dev/null || true

root_was_read_only=false
case "$(awk '$2 == "/" { print $4; exit }' /proc/mounts)" in
    ro|ro,*|*,ro|*,ro,*) root_was_read_only=true ;;
esac
if [ "$root_was_read_only" = true ]; then
    mount -o remount,rw /
fi
rm -f "$UNIT_ROOT/multi-user.target.wants/remagicd.service"
rm -f "$UNIT_ROOT/multi-user.target.wants/magicpaper-agent.service"
rm -f "$UNIT_ROOT/remagicd.service"
rm -f "$UNIT_ROOT/remagic-home.service"
rm -f "$UNIT_ROOT/remagic-app@.service"
rm -f "$UNIT_ROOT/remagic-recover.service"
rm -f "$UNIT_ROOT/magicpaper-agent.service"
if [ "$root_was_read_only" = true ]; then
    mount -o remount,ro /
fi
systemctl daemon-reload

if [ -f "$STATE_ROOT/previous.env" ]; then
    . "$STATE_ROOT/previous.env"
    if [ "${RIDDLE_POWER_LAUNCHER:-disabled}" = enabled ]; then
        systemctl enable --now riddle-power-launcher.service 2>/dev/null || true
    fi
fi

rm -rf "$APP_ROOT"
if [ -f /home/root/apps/riddle/riddle.pre-remagic ]; then
    mv /home/root/apps/riddle/riddle.pre-remagic /home/root/apps/riddle/riddle
fi
if [ "$PURGE" = "--purge" ]; then
    rm -rf /home/root/.local/state/remagic /home/root/.local/share/remagic
fi

echo "Remagic Manager removed; the original interface has been restored."
