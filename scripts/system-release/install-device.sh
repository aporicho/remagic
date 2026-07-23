#!/bin/sh
set -eu

[ "$(id -u)" -eq 0 ] || {
    echo "ReMagic installer must run as root" >&2
    exit 1
}

SOURCE_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
COMMON=$SOURCE_DIR/common.sh
[ -r "$COMMON" ] || { echo "ReMagic release helper is missing" >&2; exit 1; }
# shellcheck source=scripts/system-release/common.sh
. "$COMMON"

RELEASE_FILE=$SOURCE_DIR/release.env
CHECKSUM_FILE=$SOURCE_DIR/SHA256SUMS
[ -f "$RELEASE_FILE" ] && [ -f "$CHECKSUM_FILE" ] || {
    echo "ReMagic release metadata is incomplete" >&2
    exit 1
}
(cd "$SOURCE_DIR" && sha256sum -c SHA256SUMS)

schema=$(require_safe_release_value REMAGIC_RELEASE_SCHEMA "$RELEASE_FILE")
version=$(require_safe_release_value REMAGIC_VERSION "$RELEASE_FILE")
api=$(require_safe_release_value REMAGIC_API "$RELEASE_FILE")
release_os=$(require_safe_release_value SUPPORTED_OS_SERIES "$RELEASE_FILE")
devices=$(require_safe_release_value SUPPORTED_DEVICES "$RELEASE_FILE")
store_package=$(require_safe_release_value STORE_PACKAGE "$RELEASE_FILE")
pi_runtime_schema=$(require_safe_release_value REMAGIC_PI_RUNTIME_SCHEMA "$RELEASE_FILE")
pi_version=$(require_safe_release_value REMAGIC_PI_VERSION "$RELEASE_FILE")
node_version=$(require_safe_release_value REMAGIC_NODE_VERSION "$RELEASE_FILE")
[ "$schema" = 1 ] && [ "$api" = 5 ] || {
    echo "ReMagic release schema/API is unsupported" >&2
    exit 1
}
REMAGIC_SUPPORTED_OS_SERIES=$release_os
detect_supported_device
case ",$devices," in
    *,"$REMAGIC_DEVICE_CODENAME",*) ;;
    *) echo "ReMagic release does not support $REMAGIC_DEVICE_CODENAME" >&2; exit 1 ;;
esac

PAYLOAD=$SOURCE_DIR/payload
STORE_BUNDLE=$SOURCE_DIR/$store_package
PI_RUNTIME=$PAYLOAD/runtime/pi
[ -x "$PAYLOAD/bin/remagicd" ] && \
    [ -x "$PAYLOAD/bin/remagic-agentd" ] && \
    [ -x "$PI_RUNTIME/bin/node" ] && [ ! -L "$PI_RUNTIME/bin/node" ] && \
    [ -x "$PI_RUNTIME/bin/pi" ] && [ ! -L "$PI_RUNTIME/bin/pi" ] && \
    [ -f "$PI_RUNTIME/extensions/remagic-tools.js" ] && \
    [ ! -L "$PI_RUNTIME/extensions/remagic-tools.js" ] && \
    [ -f "$PI_RUNTIME/runtime.env" ] && [ ! -L "$PI_RUNTIME/runtime.env" ] && \
    [ -x "$PAYLOAD/bin/remagic-package-inspect" ] && \
    [ -x "$PAYLOAD/bin/remagic-update" ] && \
    [ -x "$PAYLOAD/libexec/remagic-register" ] && \
    [ -x "$PAYLOAD/libexec/remagic-configure-provider" ] && \
    [ -f "$PAYLOAD/share/system-files.sha256" ] && \
    [ -f "$PAYLOAD/share/system-trusted-keys.json" ] && \
    [ -f "$STORE_BUNDLE" ] || {
    echo "ReMagic release payload is incomplete" >&2
    exit 1
}
(cd "$PAYLOAD" && sha256sum -c share/system-files.sha256)
payload_pi_schema=$(require_safe_release_value REMAGIC_PI_RUNTIME_SCHEMA \
    "$PI_RUNTIME/runtime.env")
payload_pi_version=$(require_safe_release_value REMAGIC_PI_VERSION \
    "$PI_RUNTIME/runtime.env")
payload_node_version=$(require_safe_release_value REMAGIC_NODE_VERSION \
    "$PI_RUNTIME/runtime.env")
[ "$pi_runtime_schema" = 1 ] && \
    [ "$payload_pi_schema" = "$pi_runtime_schema" ] && \
    [ "$payload_pi_version" = "$pi_version" ] && \
    [ "$payload_node_version" = "$node_version" ] || {
    echo "ReMagic Pi runtime version does not match release metadata" >&2
    exit 1
}

APP_ROOT=/home/root/apps/remagic
APPS_ROOT=/home/root/apps
MANIFEST_ROOT=/home/root/.local/share/remagic/apps.d
PACKAGE_STATE_ROOT=/home/root/.local/state/remagic/packages
LOCK=/run/remagic-install.lock
TXN_ROOT=/home/root/.local/state/remagic/system-install
TXN=$TXN_ROOT/transaction-$$
STAGE=$APPS_ROOT/.remagic.stage.$$
BACKUP=$APPS_ROOT/.remagic.rollback.$$
COMMITTED=false
FIRST_INSTALL=false
ROOT_WRITABLE=false
ROOT_WAS_RO=false
LEGACY_AGENT_WAS_ACTIVE=false

case ",$(awk '$2 == "/" { print $4; exit }' /proc/mounts)," in
    *,ro,*) ROOT_WAS_RO=true ;;
esac

make_root_writable() {
    [ "$ROOT_WRITABLE" = true ] && return 0
    if [ "$ROOT_WAS_RO" = true ]; then
        mount -o remount,rw /
    fi
    ROOT_WRITABLE=true
}

restore_root_mount() {
    if [ "$ROOT_WAS_RO" = true ] && [ "$ROOT_WRITABLE" = true ]; then
        sync
        mount -o remount,ro /
    fi
    ROOT_WRITABLE=false
}

snapshot_path() {
    path=$1
    name=$2
    mkdir -p "$TXN/snapshots/$name"
    printf '%s\n' "$path" >"$TXN/snapshots/$name/path"
    if [ -e "$path" ] || [ -L "$path" ]; then
        cp -a "$path" "$TXN/snapshots/$name/value"
        : >"$TXN/snapshots/$name/present"
    fi
}

restore_snapshot() {
    record=$1
    path=$(sed -n '1p' "$record/path")
    case "$path" in
        /home/root/apps/remagic-store|\
        /home/root/.local/share/remagic/apps.d/*.toml|\
        /home/root/.local/state/remagic/packages/remagic-store.json|\
        /usr/lib/systemd/system/remagic*.service|\
        /usr/lib/systemd/system/remagic*.socket|\
        /usr/lib/systemd/system/remagic-app@koreader.service.d/10-koreader-runtime.conf|\
        /usr/lib/systemd/system/multi-user.target.wants/remagicd.service|\
        /usr/lib/systemd/system/sockets.target.wants/remagic-agentd.socket) ;;
        *) echo "ReMagic: unsafe rollback path $path" >&2; return 1 ;;
    esac
    rm -rf "$path"
    if [ -e "$record/present" ]; then
        mkdir -p "$(dirname "$path")"
        cp -a "$record/value" "$path"
    fi
}

restore_snapshots() {
    make_root_writable
    for record in "$TXN/snapshots/"*; do
        [ -d "$record" ] || continue
        restore_snapshot "$record"
    done
    systemctl daemon-reload >/dev/null 2>&1 || true
}

restore_stock() {
    systemctl unmask --runtime xochitl.service >/dev/null 2>&1 || true
    systemctl start xochitl.service >/dev/null 2>&1 || true
    systemctl start paperweight.service >/dev/null 2>&1 || true
}

rollback() {
    systemctl stop remagic-agentd.service remagic-agentd.socket >/dev/null 2>&1 || true
    systemctl stop remagicd.service >/dev/null 2>&1 || true
    rm -rf "$APP_ROOT"
    if [ -e "$BACKUP" ]; then
        mv "$BACKUP" "$APP_ROOT"
    fi
    restore_snapshots || true
    restore_stock
    if [ -x "$APP_ROOT/libexec/remagic-register" ]; then
        "$APP_ROOT/libexec/remagic-register" --persistent >/dev/null 2>&1 || true
    fi
    if [ "$LEGACY_AGENT_WAS_ACTIVE" = true ]; then
        systemctl start magicpaper-agent.service >/dev/null 2>&1 || true
    fi
}

finish() {
    status=$?
    trap - EXIT HUP INT TERM
    if [ "$status" -ne 0 ] && [ "$COMMITTED" = false ]; then
        echo "ReMagic: installation failed; restoring the previous system" >&2
        rollback || true
    fi
    rm -rf "$STAGE"
    restore_root_mount || status=1
    rm -rf "$LOCK"
    [ "$COMMITTED" = true ] && rm -rf "$TXN"
    exit "$status"
}
trap finish EXIT
trap 'exit 1' HUP INT TERM

if ! mkdir "$LOCK" 2>/dev/null; then
    echo "Another ReMagic install or recovery is running" >&2
    exit 1
fi
printf '%s\n' "$$" >"$LOCK/pid"
mkdir -p "$TXN/snapshots" "$APPS_ROOT" "$MANIFEST_ROOT" "$PACKAGE_STATE_ROOT"
printf '%s\n' "$version" >"$TXN/version"

for entry in \
    'store-app:/home/root/apps/remagic-store' \
    'store-manifest:/home/root/.local/share/remagic/apps.d/remagic-store.toml' \
    'store-state:/home/root/.local/state/remagic/packages/remagic-store.json' \
    'magicpaper-manifest:/home/root/.local/share/remagic/apps.d/magicpaper.toml' \
    'koreader-manifest:/home/root/.local/share/remagic/apps.d/koreader.toml' \
    'unit-daemon:/usr/lib/systemd/system/remagicd.service' \
    'unit-display:/usr/lib/systemd/system/remagic-display-host.service' \
    'unit-home:/usr/lib/systemd/system/remagic-home.service' \
    'unit-app:/usr/lib/systemd/system/remagic-app@.service' \
    'unit-recover:/usr/lib/systemd/system/remagic-recover.service' \
    'unit-agent:/usr/lib/systemd/system/remagic-agentd.service' \
    'unit-agent-socket:/usr/lib/systemd/system/remagic-agentd.socket' \
    'unit-koreader:/usr/lib/systemd/system/remagic-app@koreader.service.d/10-koreader-runtime.conf' \
    'unit-wants:/usr/lib/systemd/system/multi-user.target.wants/remagicd.service' \
    'unit-agent-wants:/usr/lib/systemd/system/sockets.target.wants/remagic-agentd.socket'; do
    name=${entry%%:*}
    path=${entry#*:}
    snapshot_path "$path" "$name"
done

if [ ! -d "$APP_ROOT" ]; then
    FIRST_INSTALL=true
fi
rm -rf "$STAGE" "$BACKUP"
mkdir -p "$STAGE"
cp -a "$PAYLOAD/." "$STAGE/"
chown -R 0:0 "$STAGE"
(cd "$STAGE" && sha256sum -c share/system-files.sha256)

# Stop only ReMagic ownership. Stock xochitl and Paperweight are restored
# before files move, so a failed installer never leaves a blank panel.
systemctl stop remagic-agentd.service remagic-agentd.socket >/dev/null 2>&1 || true
systemctl stop remagicd.service >/dev/null 2>&1 || true
restore_stock
if systemctl is-active --quiet magicpaper-agent.service; then
    LEGACY_AGENT_WAS_ACTIVE=true
    systemctl stop magicpaper-agent.service || {
        echo "ReMagic: could not stop the retired MagicPaper agent" >&2
        exit 1
    }
fi
if [ -d "$APP_ROOT" ]; then
    mv "$APP_ROOT" "$BACKUP"
fi
mv "$STAGE" "$APP_ROOT"
cp "$RELEASE_FILE" "$APP_ROOT/share/release.env"
chmod 0644 "$APP_ROOT/share/release.env"

# Remove only manifests from the retired monolithic layout. Store-installed
# applications use their own `/home/root/apps/<id>/current` paths and survive.
if grep -q '/home/root/apps/remagic/opt/magicpaper' "$MANIFEST_ROOT/magicpaper.toml" 2>/dev/null; then
    rm -f "$MANIFEST_ROOT/magicpaper.toml"
fi
if grep -Eq '/home/root/apps/(koreader-for-remagic|remagic/opt/koreader)' \
    "$MANIFEST_ROOT/koreader.toml" 2>/dev/null; then
    rm -f "$MANIFEST_ROOT/koreader.toml"
fi

REMAGIC_APPS_ROOT=$APPS_ROOT \
REMAGIC_MANIFEST_ROOT=$MANIFEST_ROOT \
REMAGIC_PACKAGE_STATE_ROOT=$PACKAGE_STATE_ROOT \
    "$APP_ROOT/bin/remagic-package-inspect" install "$STORE_BUNDLE" \
        "$REMAGIC_DEVICE_PRODUCT" "$REMAGIC_OS_VERSION"

# Seed the verified offline catalog before first launch. Normal Store use still
# refreshes over HTTPS, but a temporarily offline tablet can immediately show
# the two first-party applications from the signed catalog shipped with this
# system release.
STORE_PAYLOAD=/home/root/apps/remagic-store/current/payload
REMAGIC_STORE_CATALOG_DIR=$STORE_PAYLOAD/share/catalog \
    "$STORE_PAYLOAD/bin/remagic-store" catalog \
        "$REMAGIC_DEVICE_PRODUCT" "$REMAGIC_OS_VERSION" >/dev/null

# Retire the monolithic agent only after its process has stopped, but before
# daemon-reload follows links into the replaced system tree. A rollback uses
# the restored release's register helper to republish and restart it.
make_root_writable
systemctl disable magicpaper-agent.service >/dev/null 2>&1 || true
rm -f /run/systemd/system/magicpaper-agent.service \
    /run/systemd/system/multi-user.target.wants/magicpaper-agent.service \
    /usr/lib/systemd/system/magicpaper-agent.service \
    /usr/lib/systemd/system/multi-user.target.wants/magicpaper-agent.service
systemctl daemon-reload

"$APP_ROOT/libexec/remagic-register" --persistent
wait_for_remagic_ready "$APP_ROOT/bin/remagicctl"

if [ "$FIRST_INSTALL" = true ]; then
    rm -f /home/root/.local/state/remagic/welcome-v1
fi

rm -rf "$APPS_ROOT/.remagic.previous"
if [ -e "$BACKUP" ]; then
    mv "$BACKUP" "$APPS_ROOT/.remagic.previous"
fi
COMMITTED=true
sync
echo "ReMagic $version installed for $REMAGIC_DEVICE_PRODUCT ($REMAGIC_OS_VERSION)."
