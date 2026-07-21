#!/bin/sh
set -eu

[ "$(id -u)" -eq 0 ] || { echo "uninstall-device.sh must run as root" >&2; exit 1; }
case ${1:-} in ''|--purge) ;; *) echo "usage: uninstall-device.sh [--purge]" >&2; exit 1 ;; esac

PURGE=${1:-}
APP_ROOT=/home/root/apps/remagic
ADAPTER_ROOT=/home/root/apps/remagic-koreader
STATE_ROOT=/home/root/.local/state/remagic/install
ORIGINAL_ROOT=$STATE_ROOT/original
MANIFEST_ROOT=/home/root/.local/share/remagic/apps.d
UNIT_ROOT=/usr/lib/systemd/system
WANTS_ROOT=/etc/systemd/system/multi-user.target.wants
IN_PROGRESS=$STATE_ROOT/in-progress
COMMON=$APP_ROOT/libexec/deployment-common.sh
REMAGIC_ACCEPTANCE_RECOVERY_LIB=$APP_ROOT/libexec/device-test-recovery.sh
[ -r "$COMMON" ] || { echo "uninstall-device.sh: deployment helper is missing" >&2; exit 1; }
. "$COMMON"
umask 077

ROOT_WAS_READ_ONLY=false
ROOT_IS_WRITABLE=false
LOCK_HELD=false
UNINSTALL_COMMITTED=false
CURRENT_TXN=

detect_root_mount() {
    case "$(awk '$2 == "/" { print $4; exit }' /proc/mounts)" in
        ro|ro,*|*,ro|*,ro,*) ROOT_WAS_READ_ONLY=true ;;
    esac
}

ensure_root_writable() {
    [ "$ROOT_IS_WRITABLE" = true ] && return 0
    [ "$ROOT_WAS_READ_ONLY" = false ] || mount -o remount,rw /
    ROOT_IS_WRITABLE=true
}

restore_root_mount() {
    if [ "$ROOT_WAS_READ_ONLY" = true ] && [ "$ROOT_IS_WRITABLE" = true ]; then
        mount -o remount,ro / 2>/dev/null || return 1
    fi
    ROOT_IS_WRITABLE=false
}

transaction_status() {
    sed -n '1p' "$1/status" 2>/dev/null || true
}

restore_all_snapshots() {
    root=$1
    for record in "$root/"*; do
        [ -d "$record" ] || continue
        restore_snapshot "$root" "$(basename "$record")" || return 1
    done
}

restore_switch() {
    txn=$1
    name=$2
    record=$txn/switch-$name
    [ -e "$record/started" ] || return 0
    live=$(sed -n '1p' "$record/live")
    stage=$(sed -n '1p' "$record/stage")
    backup=$(sed -n '1p' "$record/backup")
    canonical_absolute_path "$live" && canonical_absolute_path "$stage" && \
        canonical_absolute_path "$backup" || return 1
    case "$name:$live:$stage:$backup" in
        app:/home/root/apps/remagic:/home/root/apps/.remagic.stage.*:/home/root/apps/.remagic.rollback.*|\
        adapter:/home/root/apps/remagic-koreader:/home/root/apps/.remagic-koreader.stage.*:/home/root/apps/.remagic-koreader.rollback.*|\
        koreader:/home/root/apps/koreader:/home/root/apps/.koreader.stage.*:/home/root/apps/.koreader.rollback.*) ;;
        *) echo "uninstall-device.sh: refusing unsafe switch journal: $record" >&2; return 1 ;;
    esac
    rollback_tree_switch "$record" "$live" "$stage" "$backup"
}

restore_removed_tree() {
    txn=$1
    name=$2
    record=$txn/remove-$name
    [ -r "$record/live" ] || return 0
    live=$(sed -n '1p' "$record/live")
    backup=$(sed -n '1p' "$record/backup")
    canonical_absolute_path "$live" && canonical_absolute_path "$backup" || return 1
    case "$name:$live:$backup" in
        app:/home/root/apps/remagic:/home/root/apps/.remagic.uninstall.*|\
        adapter:/home/root/apps/remagic-koreader:/home/root/apps/.remagic-koreader.uninstall.*) ;;
        *) echo "uninstall-device.sh: refusing unsafe removal journal: $record" >&2; return 1 ;;
    esac
    if [ -e "$backup" ] || [ -L "$backup" ]; then
        rm -rf "$live"
        mv "$backup" "$live"
    fi
}

rollback_deployment_transaction() {
    txn=$1
    [ -d "$txn" ] || return 0
    case "$(transaction_status "$txn")" in committed|rolled-back) return 0 ;; esac
    echo "uninstall-device.sh: rolling back incomplete deployment $(basename "$txn")" >&2
    stop_unit_confirmed magicpaper-agent.service || return 1
    stop_unit_confirmed remagicd.service || return 1
    stop_alternative_display_owners || return 1
    stop_unit_confirmed paperweight.service || return 1
    assert_no_known_owner_processes || return 1
    remove_stale_qtfb_surfaces || return 1
    kind=$(sed -n '1p' "$txn/kind" 2>/dev/null || true)
    case "$kind" in
        ''|install)
            restore_switch "$txn" app || return 1
            restore_switch "$txn" adapter || return 1
            restore_switch "$txn" koreader || return 1
            ;;
        uninstall)
            restore_removed_tree "$txn" app || return 1
            restore_removed_tree "$txn" adapter || return 1
            ;;
        *) echo "uninstall-device.sh: unknown transaction kind: $kind" >&2; return 1 ;;
    esac
    restore_all_snapshots "$txn/snapshots" || return 1
    "$SYSTEMCTL" daemon-reload >/dev/null 2>&1 || return 1
    restore_stock_services || return 1
    [ ! -e "$txn/was-remagicd-active" ] || "$SYSTEMCTL" start remagicd.service >/dev/null 2>&1 || return 1
    [ ! -e "$txn/was-agent-active" ] || "$SYSTEMCTL" start magicpaper-agent.service >/dev/null 2>&1 || return 1
    [ ! -e "$txn/was-riddle-active" ] || "$SYSTEMCTL" start riddle-power-launcher.service >/dev/null 2>&1 || return 1
    printf '%s\n' rolled-back >"$txn/status"
    sync
    clear_deployment_guard_for_transaction "$IN_PROGRESS" "$txn"
}

finish_uninstall() {
    status=$?
    trap - EXIT HUP INT TERM
    set +e
    if [ "$status" -ne 0 ] && [ -n "$CURRENT_TXN" ] && [ "$UNINSTALL_COMMITTED" = false ]; then
        ensure_root_writable || status=1
        rollback_deployment_transaction "$CURRENT_TXN" || status=1
    fi
    restore_root_mount || status=1
    [ "$LOCK_HELD" = false ] || release_directory_lock "$REMAGIC_INSTALL_LOCK"
    exit "$status"
}
trap finish_uninstall EXIT
trap 'exit 1' HUP INT TERM

snapshot_managed_state() {
    snapshots=$1
    mkdir -p "$snapshots"
    snapshot_path "$snapshots" unit_remagicd "$UNIT_ROOT/remagicd.service"
    snapshot_path "$snapshots" unit_display "$UNIT_ROOT/remagic-display-host.service"
    snapshot_path "$snapshots" unit_home "$UNIT_ROOT/remagic-home.service"
    snapshot_path "$snapshots" unit_runtime "$UNIT_ROOT/remagic-runtime.service"
    snapshot_path "$snapshots" unit_app "$UNIT_ROOT/remagic-app@.service"
    snapshot_path "$snapshots" unit_recover "$UNIT_ROOT/remagic-recover.service"
    snapshot_path "$snapshots" unit_agent "$UNIT_ROOT/magicpaper-agent.service"
    snapshot_path "$snapshots" want_remagicd "$WANTS_ROOT/remagicd.service"
    snapshot_path "$snapshots" want_agent "$WANTS_ROOT/magicpaper-agent.service"
    snapshot_path "$snapshots" want_old_remagicd "$UNIT_ROOT/multi-user.target.wants/remagicd.service"
    snapshot_path "$snapshots" want_old_agent "$UNIT_ROOT/multi-user.target.wants/magicpaper-agent.service"
    snapshot_path "$snapshots" want_runtime_etc "$WANTS_ROOT/remagic-runtime.service"
    snapshot_path "$snapshots" want_runtime_usr "$UNIT_ROOT/multi-user.target.wants/remagic-runtime.service"
    snapshot_path "$snapshots" want_riddle_usr "$UNIT_ROOT/multi-user.target.wants/riddle-power-launcher.service"
    snapshot_path "$snapshots" want_riddle_etc "$WANTS_ROOT/riddle-power-launcher.service"
    snapshot_path "$snapshots" manifest_magicpaper "$MANIFEST_ROOT/magicpaper.toml"
    snapshot_path "$snapshots" manifest_koreader "$MANIFEST_ROOT/koreader.toml"
    snapshot_path "$snapshots" riddle_binary /home/root/apps/riddle/riddle
    snapshot_path "$snapshots" riddle_backup /home/root/apps/riddle/riddle.pre-remagic
}

remove_tree_transactionally() {
    txn=$1
    name=$2
    live=$3
    backup=$4
    record=$txn/remove-$name
    mkdir -p "$record"
    printf '%s\n' "$live" >"$record/live"
    printf '%s\n' "$backup" >"$record/backup"
    sync
    rm -rf "$backup"
    [ ! -e "$live" ] && [ ! -L "$live" ] || mv "$live" "$backup"
}

restore_original_launcher_links() {
    rm -f "$UNIT_ROOT/multi-user.target.wants/riddle-power-launcher.service" \
        "$WANTS_ROOT/riddle-power-launcher.service"
    if [ -e "$ORIGINAL_ROOT/captured" ]; then
        restore_snapshot "$ORIGINAL_ROOT/snapshots" riddle_want_usr
        restore_snapshot "$ORIGINAL_ROOT/snapshots" riddle_want_etc
        enabled=$(sed -n '1p' "$ORIGINAL_ROOT/launcher-enabled" 2>/dev/null || true)
        if [ "$enabled" = enabled ] && \
           [ ! -L "$UNIT_ROOT/multi-user.target.wants/riddle-power-launcher.service" ] && \
           [ ! -L "$WANTS_ROOT/riddle-power-launcher.service" ]; then
            mkdir -p "$WANTS_ROOT"
            ln -s /usr/lib/systemd/system/riddle-power-launcher.service \
                "$WANTS_ROOT/riddle-power-launcher.service"
        fi
        return 0
    fi
    if [ -r "$STATE_ROOT/previous.env" ] && grep -q '^RIDDLE_POWER_LAUNCHER=enabled$' "$STATE_ROOT/previous.env"; then
        mkdir -p "$WANTS_ROOT"
        ln -s /usr/lib/systemd/system/riddle-power-launcher.service \
            "$WANTS_ROOT/riddle-power-launcher.service"
    fi
}

original_launcher_should_start() {
    [ -e "$ORIGINAL_ROOT/launcher-active" ] && return 0
    if [ ! -e "$ORIGINAL_ROOT/captured" ] && [ -r "$STATE_ROOT/previous.env" ]; then
        grep -q '^RIDDLE_POWER_LAUNCHER=enabled$' "$STATE_ROOT/previous.env"
        return
    fi
    return 1
}

acquire_directory_lock "$REMAGIC_INSTALL_LOCK" uninstall-device.sh
LOCK_HELD=true
wait_for_lock_barrier "$REMAGIC_RECOVERY_LOCK" uninstall-device.sh
cleanup_stale_acceptance_environment
detect_root_mount
ensure_root_writable
mkdir -p "$STATE_ROOT/transactions"
transaction_names=$(list_deployment_transactions_newest_first "$STATE_ROOT/transactions")
for transaction_name in $transaction_names; do
    abandoned=$STATE_ROOT/transactions/$transaction_name
    case "$(transaction_status "$abandoned")" in
        committed|rolled-back)
            cleanup_finished_deployment_transaction "$abandoned"
            clear_deployment_guard_for_transaction "$IN_PROGRESS" "$abandoned"
            continue
            ;;
    esac
    rollback_deployment_transaction "$abandoned"
done
TXN_ID=uninstall-$(date +%Y%m%d-%H%M%S 2>/dev/null || echo unknown)-$$
CURRENT_TXN=$STATE_ROOT/transactions/$TXN_ID
mkdir -p "$CURRENT_TXN"
printf '%s\n' removing >"$CURRENT_TXN/status"
printf '%s\n' uninstall >"$CURRENT_TXN/kind"
printf '%s\n' "$TXN_ID" >"$IN_PROGRESS"
sync
unit_is_active remagicd.service && : >"$CURRENT_TXN/was-remagicd-active"
unit_is_active magicpaper-agent.service && : >"$CURRENT_TXN/was-agent-active"
unit_is_active riddle-power-launcher.service && : >"$CURRENT_TXN/was-riddle-active"
snapshot_managed_state "$CURRENT_TXN/snapshots"
sync

if [ -x "$APP_ROOT/bin/remagicctl" ]; then
    "$APP_ROOT/bin/remagicctl" system >/dev/null 2>&1 || true
fi
stop_unit_confirmed magicpaper-agent.service
stop_unit_confirmed remagicd.service
stop_alternative_display_owners
stop_unit_confirmed paperweight.service
assert_no_known_owner_processes
remove_stale_qtfb_surfaces
remove_manager_runtime_files
restore_xochitl_service

remove_tree_transactionally "$CURRENT_TXN" app "$APP_ROOT" \
    /home/root/apps/.remagic.uninstall.$TXN_ID
remove_tree_transactionally "$CURRENT_TXN" adapter "$ADAPTER_ROOT" \
    /home/root/apps/.remagic-koreader.uninstall.$TXN_ID

if [ -f /home/root/apps/riddle/riddle.pre-remagic ]; then
    rm -f /home/root/apps/riddle/riddle
    mv /home/root/apps/riddle/riddle.pre-remagic /home/root/apps/riddle/riddle
fi

rm -f "$WANTS_ROOT/remagicd.service" "$WANTS_ROOT/magicpaper-agent.service" \
    "$WANTS_ROOT/remagic-runtime.service" \
    "$UNIT_ROOT/multi-user.target.wants/remagicd.service" \
    "$UNIT_ROOT/multi-user.target.wants/magicpaper-agent.service" \
    "$UNIT_ROOT/multi-user.target.wants/remagic-runtime.service"
rm -f "$UNIT_ROOT/remagicd.service" "$UNIT_ROOT/remagic-display-host.service" \
    "$UNIT_ROOT/remagic-home.service" "$UNIT_ROOT/remagic-runtime.service" \
    "$UNIT_ROOT/remagic-app@.service" "$UNIT_ROOT/remagic-recover.service" \
    "$UNIT_ROOT/magicpaper-agent.service"
rm -f "$MANIFEST_ROOT/magicpaper.toml" "$MANIFEST_ROOT/koreader.toml"
restore_original_launcher_links

"$SYSTEMCTL" daemon-reload
restore_stock_services
if original_launcher_should_start || [ -e "$CURRENT_TXN/was-riddle-active" ]; then
    start_if_loaded riddle-power-launcher.service || true
fi
sync
printf '%s\n' committed >"$CURRENT_TXN/status"
sync
UNINSTALL_COMMITTED=true

for name in app adapter; do
    backup=$(sed -n '1p' "$CURRENT_TXN/remove-$name/backup" 2>/dev/null || true)
    canonical_absolute_path "$backup" || {
        echo "uninstall-device.sh: refusing non-canonical cleanup journal" >&2
        exit 1
    }
    case "$name:$backup" in
        app:/home/root/apps/.remagic.uninstall.*|\
        adapter:/home/root/apps/.remagic-koreader.uninstall.*) ;;
        *) echo "uninstall-device.sh: refusing unsafe cleanup journal" >&2; exit 1 ;;
    esac
    [ -z "$backup" ] || rm -rf "$backup"
done
clear_deployment_guard_for_transaction "$IN_PROGRESS" "$CURRENT_TXN"
rm -rf "$ORIGINAL_ROOT"
if [ "$PURGE" = --purge ]; then
    rm -rf /home/root/.local/state/remagic /home/root/.local/share/remagic
else
    rm -rf "$CURRENT_TXN/snapshots"
fi

restore_root_mount
release_directory_lock "$REMAGIC_INSTALL_LOCK"
LOCK_HELD=false
echo "Remagic Manager removed; stock and Paperweight ownership were restored."
echo "MagicPaper and KOReader user data were preserved."
