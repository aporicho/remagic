#!/bin/sh

# Shared, POSIX-sh deployment primitives.  Callers deliberately choose when a
# failure is fatal; these helpers never delete vendor framebuffer locks.

SYSTEMCTL=${SYSTEMCTL:-systemctl}
REMAGIC_INSTALL_LOCK=${REMAGIC_INSTALL_LOCK:-/run/remagic-install.lock}
REMAGIC_RECOVERY_LOCK=${REMAGIC_RECOVERY_LOCK:-/run/remagic-recover.lock}
REMAGIC_ACCEPTANCE_LOCK=${REMAGIC_ACCEPTANCE_LOCK:-/run/remagic-acceptance.lock}
REMAGIC_SYSTEMD_RUNTIME_ROOT=${REMAGIC_SYSTEMD_RUNTIME_ROOT:-/run/systemd/system}
REMAGIC_ACCEPTANCE_STALE_CLEANED=false

lock_owner_is_live() {
    owner_file=$1/pid
    lock_owner=$(sed -n '1p' "$owner_file" 2>/dev/null || true)
    case "$lock_owner" in
        ''|*[!0-9]*|0|1) return 1 ;;
        *) kill -0 "$lock_owner" 2>/dev/null ;;
    esac
}

cleanup_stale_acceptance_environment() {
    live_policy=${1:-refuse-live}
    daemon_dropin=$REMAGIC_SYSTEMD_RUNTIME_ROOT/remagicd.service.d/90-remagic-acceptance.conf
    runner_dropin=$REMAGIC_SYSTEMD_RUNTIME_ROOT/remagic-app@.service.d/90-remagic-acceptance.conf
    if [ -d "$REMAGIC_ACCEPTANCE_LOCK" ]; then
        if lock_owner_is_live "$REMAGIC_ACCEPTANCE_LOCK"; then
            [ "$live_policy" = allow-live ] && return 0
            echo "deployment: acceptance test is active; refusing to race it" >&2
            return 1
        fi
        case "$lock_owner" in ''|*[!0-9]*|0|1)
            echo "deployment: stale acceptance PID cannot be proven dead" >&2
            return 1
            ;;
        esac
        [ -r "${REMAGIC_ACCEPTANCE_RECOVERY_LIB:-}" ] || {
            echo "deployment: acceptance recovery helper is unavailable" >&2
            return 1
        }
        REMAGIC_TEST_LOCK=$REMAGIC_ACCEPTANCE_LOCK
        REMAGIC_TEST_ROOT=${REMAGIC_ACCEPTANCE_TEST_ROOT:-/home/root/.local/state/remagic/acceptance/current}
        REMAGIC_TEST_SESSION_ROOT=${REMAGIC_ACCEPTANCE_SESSION_ROOT:-/home/root/.local/state/remagic/sessions}
        REMAGIC_TEST_SYSTEMD_RUNTIME_ROOT=$REMAGIC_SYSTEMD_RUNTIME_ROOT
        REMAGIC_TEST_SYSTEMCTL=$SYSTEMCTL
        REMAGIC_TEST_CTL=${REMAGIC_ACCEPTANCE_CTL:-/home/root/apps/remagic/bin/remagicctl}
        REMAGIC_TEST_OVERRIDES_INSTALLED=false
        REMAGIC_TEST_SESSIONS_SAVED=false
        . "$REMAGIC_ACCEPTANCE_RECOVERY_LIB"
        remagic_acceptance_recover_orphan "$lock_owner" || return 1
        REMAGIC_ACCEPTANCE_STALE_CLEANED=true
        return 0
    fi
    if [ -e "$daemon_dropin" ] || [ -L "$daemon_dropin" ] || \
       [ -e "$runner_dropin" ] || [ -L "$runner_dropin" ]; then
        echo "deployment: acceptance override exists without a lock; refusing unproven cleanup" >&2
        return 1
    fi
}

unit_is_active() {
    [ "$("$SYSTEMCTL" is-active "$1" 2>/dev/null || true)" = active ]
}

unit_is_loaded() {
    [ "$("$SYSTEMCTL" show --property=LoadState --value "$1" 2>/dev/null || true)" = loaded ]
}

wait_unit_inactive() {
    unit=$1
    attempts=${2:-80}
    while [ "$attempts" -gt 0 ]; do
        unit_is_active "$unit" || return 0
        sleep 0.1
        attempts=$((attempts - 1))
    done
    return 1
}

wait_unit_active() {
    unit=$1
    attempts=${2:-80}
    while [ "$attempts" -gt 0 ]; do
        unit_is_active "$unit" && return 0
        sleep 0.1
        attempts=$((attempts - 1))
    done
    return 1
}

list_remagic_app_units() {
    "$SYSTEMCTL" list-units --all --plain --no-legend 'remagic-app@*.service' 2>/dev/null \
        | awk '$1 ~ /^remagic-app@.*\.service$/ { print $1 }'
}

list_legacy_display_units() {
    "$SYSTEMCTL" list-units --all --plain --no-legend \
        'remagic-runtime*.service' 'riddle*.service' 'appload*.service' \
        'rm-appload*.service' 'koreader.service' 'magicpaper*.service' 2>/dev/null \
        | awk '$1 ~ /^(remagic-runtime.*|riddle.*|appload.*|rm-appload.*|koreader|magicpaper.*)\.service$/ { print $1 }'
}

stop_unit_confirmed() {
    unit=$1
    if unit_is_loaded "$unit" || unit_is_active "$unit"; then
        "$SYSTEMCTL" stop "$unit" >/dev/null 2>&1 || true
    fi
    wait_unit_inactive "$unit" 50 && return 0
    "$SYSTEMCTL" kill --kill-who=all --signal=KILL "$unit" >/dev/null 2>&1 || true
    "$SYSTEMCTL" stop "$unit" >/dev/null 2>&1 || true
    wait_unit_inactive "$unit" 50
}

stop_remagic_app_instances() {
    units=$(list_remagic_app_units)
    [ -z "$units" ] && return 0
    for unit in $units; do
        stop_unit_confirmed "$unit" || {
            echo "deployment: application unit remains active: $unit" >&2
            return 1
        }
    done
}

stop_alternative_display_owners() {
    stop_remagic_app_instances || return 1
    legacy_units=$(list_legacy_display_units)
    for unit in $legacy_units \
        remagic-runtime.service \
        remagic-display-host.service \
        remagic-home.service \
        magicpaper-takeover.service \
        magicpaper-power-launcher.service \
        riddle-takeover.service \
        riddle-power-launcher.service \
        appload.service \
        rm-appload.service; do
        stop_unit_confirmed "$unit" || {
            echo "deployment: alternative display owner remains active: $unit" >&2
            return 1
        }
    done
}

known_display_owner_active() {
    units=$(list_remagic_app_units)
    for unit in $units; do
        unit_is_active "$unit" && return 0
    done
    legacy_units=$(list_legacy_display_units)
    for unit in $legacy_units \
        remagic-runtime.service \
        remagic-display-host.service \
        remagic-home.service \
        magicpaper-takeover.service \
        magicpaper-power-launcher.service \
        riddle-takeover.service \
        riddle-power-launcher.service \
        appload.service \
        rm-appload.service; do
        unit_is_active "$unit" && return 0
    done
    return 1
}

assert_no_known_owner_processes() {
    for entry in /proc/[0-9]*/cmdline; do
        [ -r "$entry" ] || continue
        command_line=$(tr '\000' ' ' <"$entry" 2>/dev/null || true)
        case "$command_line" in
            *remagic-display-host*|*remagic-home*|*remagic-appload-runtime*|\
            *riddle-takeover*|*/apps/riddle/riddle*|*/magicpaper*|\
            */reader.lua*|*/appload*|*qtfb-shim*)
                echo "deployment: unmanaged display process remains: $command_line" >&2
                return 1
                ;;
        esac
    done
    return 0
}

remove_stale_qtfb_surfaces() {
    known_display_owner_active && {
        echo "deployment: refusing QTFB cleanup while a known owner is active" >&2
        return 1
    }
    assert_no_known_owner_processes || return 1
    for surface in /dev/shm/qtfb_*; do
        [ -e "$surface" ] || continue
        name=${surface##*/qtfb_}
        case "$name" in ''|*[!0-9]*)
            echo "deployment: refusing unknown QTFB object: $surface" >&2
            return 1
        esac
        [ -f "$surface" ] && [ ! -L "$surface" ] && \
            [ "$(stat -c %u "$surface" 2>/dev/null || echo unknown)" = 0 ] || {
            echo "deployment: refusing unsafe QTFB object: $surface" >&2
            return 1
        }
    done
    for surface in /dev/shm/qtfb_*; do
        [ -e "$surface" ] || continue
        rm -f "$surface"
    done
}

reset_failed_if_loaded() {
    unit=$1
    unit_is_loaded "$unit" || return 0
    "$SYSTEMCTL" reset-failed "$unit" >/dev/null 2>&1
}

start_if_loaded() {
    unit=$1
    unit_is_loaded "$unit" || return 0
    "$SYSTEMCTL" start "$unit" >/dev/null 2>&1
}

restore_xochitl_service() {
    "$SYSTEMCTL" unmask --runtime xochitl.service >/dev/null 2>&1 || true
    reset_failed_if_loaded xochitl.service || return 1
    start_if_loaded xochitl.service || return 1
    wait_unit_active xochitl.service 100 || return 1
}

restore_stock_services() {
    restore_xochitl_service || return 1
    reset_failed_if_loaded paperweight.service || return 1
    start_if_loaded paperweight.service || return 1
    return 0
}

stock_handoff_is_complete() {
    unit_is_active xochitl.service || return 1
    known_display_owner_active && return 1
    assert_no_known_owner_processes || return 1
    [ ! -e /run/remagic/managed-domain ] || return 1
    return 0
}

remove_display_runtime_files() {
    # These paths are owned by ReMagic services.  In particular, do not remove
    # /tmp/epframebuffer.lock or wildcard /tmp/qtfb-* paths: ownership of those
    # cannot be proven from a filename.
    rm -f /tmp/qtfb.sock \
        /run/remagic/display.sock \
        /run/remagic/display.lock \
        /run/remagic/managed-domain \
        /run/remagic/foreground-app \
        /run/magicpaper-koreader.request
}

remove_manager_runtime_files() {
    remove_display_runtime_files
    rm -f /run/remagic/runtime-app.sock
}

acquire_directory_lock() {
    lock_path=$1
    label=$2
    if mkdir "$lock_path" 2>/dev/null; then
        printf '%s\n' "$$" >"$lock_path/pid"
        printf '%s\n' "$label" >"$lock_path/owner"
        return 0
    fi
    lock_pid=$(sed -n '1p' "$lock_path/pid" 2>/dev/null || true)
    case "$lock_pid" in
        ''|*[!0-9]*|0|1) lock_live=false ;;
        *) if kill -0 "$lock_pid" 2>/dev/null; then lock_live=true; else lock_live=false; fi ;;
    esac
    if [ "$lock_live" = false ]; then
        rm -f "$lock_path/pid" "$lock_path/owner"
        rmdir "$lock_path" 2>/dev/null || return 1
        mkdir "$lock_path" 2>/dev/null || return 1
        printf '%s\n' "$$" >"$lock_path/pid"
        printf '%s\n' "$label" >"$lock_path/owner"
        return 0
    fi
    echo "$label: another deployment transaction is active (pid=$lock_pid)" >&2
    return 1
}

release_directory_lock() {
    lock_path=$1
    owner=$(sed -n '1p' "$lock_path/pid" 2>/dev/null || true)
    [ "$owner" = "$$" ] || return 0
    rm -f "$lock_path/pid" "$lock_path/owner"
    rmdir "$lock_path" 2>/dev/null || true
}

wait_for_lock_barrier() {
    barrier_path=$1
    barrier_label=$2
    barrier_attempts=${3:-300}
    while [ -d "$barrier_path" ] && lock_owner_is_live "$barrier_path"; do
        [ "$barrier_attempts" -gt 0 ] || {
            echo "$barrier_label: timed out waiting for $(basename "$barrier_path")" >&2
            return 1
        }
        sleep 0.1
        barrier_attempts=$((barrier_attempts - 1))
    done
    acquire_directory_lock "$barrier_path" "$barrier_label" || return 1
    release_directory_lock "$barrier_path"
}

canonical_absolute_path() {
    candidate_path=$1
    case "$candidate_path" in /*) ;; *) return 1 ;; esac
    relative_path=${candidate_path#/}
    case "/$relative_path/" in */../*|*/./*|*//* ) return 1 ;; esac
    return 0
}

cleanup_finished_deployment_transaction() {
    finished_txn=$1
    finished_status=$(sed -n '1p' "$finished_txn/status" 2>/dev/null || true)
    case "$finished_status" in committed|rolled-back) ;; *) return 1 ;; esac
    finished_kind=$(sed -n '1p' "$finished_txn/kind" 2>/dev/null || true)
    if [ "$finished_status" = committed ]; then
        case "$finished_kind" in
            ''|install)
                for finished_record in "$finished_txn"/switch-*; do
                    [ -r "$finished_record/backup" ] || continue
                    finished_path=$(sed -n '1p' "$finished_record/backup")
                    canonical_absolute_path "$finished_path" || return 1
                    case "$finished_path" in
                        /home/root/apps/.remagic.rollback.*|/home/root/apps/.koreader-for-remagic.rollback.*|/home/root/apps/.remagic-koreader.rollback.*|/home/root/apps/.koreader.rollback.*) ;;
                        *) return 1 ;;
                    esac
                    rm -rf "$finished_path"
                done
                ;;
            uninstall)
                for finished_record in "$finished_txn"/remove-*; do
                    [ -r "$finished_record/backup" ] || continue
                    finished_path=$(sed -n '1p' "$finished_record/backup")
                    canonical_absolute_path "$finished_path" || return 1
                    case "$finished_path" in
                        /home/root/apps/.remagic.uninstall.*|/home/root/apps/.koreader-for-remagic.uninstall.*|/home/root/apps/.remagic-koreader.uninstall.*) ;;
                        *) return 1 ;;
                    esac
                    rm -rf "$finished_path"
                done
                ;;
            *) return 1 ;;
        esac
    fi
    rm -rf "$finished_txn/snapshots"
}

transactional_tree_switch() {
    tree_record=$1
    tree_live=$2
    tree_stage=$3
    tree_backup=$4
    mkdir -p "$tree_record"
    printf '%s\n' "$tree_live" >"$tree_record/live"
    printf '%s\n' "$tree_stage" >"$tree_record/stage"
    printf '%s\n' "$tree_backup" >"$tree_record/backup"
    : >"$tree_record/started"
    sync
    rm -rf "$tree_backup"
    if [ -e "$tree_live" ] || [ -L "$tree_live" ]; then
        : >"$tree_record/old-present"
        sync
        mv "$tree_live" "$tree_backup"
        sync
    fi
    mv "$tree_stage" "$tree_live"
    sync
}

rollback_tree_switch() {
    tree_record=$1
    tree_live=$2
    tree_stage=$3
    tree_backup=$4
    if [ -e "$tree_backup" ] || [ -L "$tree_backup" ]; then
        rm -rf "$tree_live"
        mv "$tree_backup" "$tree_live"
    elif [ ! -e "$tree_record/old-present" ]; then
        rm -rf "$tree_live"
    fi
    rm -rf "$tree_stage"
    sync
}

deployment_home_space_required() {
    bundle_kb=$1
    active_data_kb=$2
    existing_backups_kb=$3
    legacy_data_kb=$4
    # Active data is duplicated by the rollback snapshot and the migrator's
    # own pre-change backup. Existing backups need one rollback copy, while
    # safe legacy data may be copied once into the independent data root.
    printf '%s\n' "$((bundle_kb + (2 * active_data_kb) + existing_backups_kb + legacy_data_kb + 65536))"
}

deployment_home_space_sufficient() {
    required_kb=$1
    available_kb=$2
    case "$required_kb:$available_kb" in *[!0-9:]*) return 1 ;; esac
    [ "$available_kb" -ge "$required_kb" ]
}

deployment_guard_is_incomplete() {
    guard_path=$1
    transaction_root=$2
    [ -e "$guard_path" ] || return 1
    guard_id=$(sed -n '1p' "$guard_path" 2>/dev/null || true)
    case "$guard_id" in ''|*[!A-Za-z0-9_.-]*) return 0 ;; esac
    guard_status=$(sed -n '1p' "$transaction_root/$guard_id/status" 2>/dev/null || true)
    case "$guard_status" in committed|rolled-back) return 1 ;; *) return 0 ;; esac
}

clear_deployment_guard_for_transaction() {
    guard_path=$1
    transaction_path=$2
    [ "$(sed -n '1p' "$guard_path" 2>/dev/null || true)" = "${transaction_path##*/}" ] || return 0
    rm -f "$guard_path"
    sync
}

# Nested transactions can exist only after an older installer failed to
# recover its predecessor. Always unwind newest-first so every rollback sees
# the exact tree that it originally moved aside.
list_deployment_transactions_newest_first() {
    transaction_root=$1
    for transaction_path in "$transaction_root"/*; do
        [ -d "$transaction_path" ] || continue
        transaction_name=${transaction_path##*/}
        case "$transaction_name" in
            ''|*[!A-Za-z0-9_.-]*)
                echo "deployment: unsafe transaction directory: $transaction_path" >&2
                return 1
                ;;
        esac
    done
    for transaction_path in "$transaction_root"/*; do
        [ -d "$transaction_path" ] || continue
        transaction_name=${transaction_path##*/}
        printf '%s\n' "$transaction_name"
    done | LC_ALL=C sort -r
}

snapshot_record_is_complete() {
    snapshot_root=$1
    label=$2
    expected_path=$3
    snapshot_record=$snapshot_root/$label
    [ -d "$snapshot_record" ] && [ ! -L "$snapshot_record" ] || return 1
    [ -f "$snapshot_record/path" ] && [ ! -L "$snapshot_record/path" ] || return 1
    [ -f "$snapshot_record/complete" ] && [ ! -L "$snapshot_record/complete" ] || return 1
    [ "$(sed -n '1p' "$snapshot_record/path" 2>/dev/null || true)" = "$expected_path" ] || return 1
    if [ -e "$snapshot_record/present" ] || [ -L "$snapshot_record/present" ]; then
        [ -f "$snapshot_record/present" ] && [ ! -L "$snapshot_record/present" ] || return 1
        [ -e "$snapshot_record/value" ] || [ -L "$snapshot_record/value" ] || return 1
    else
        [ ! -e "$snapshot_record/value" ] && [ ! -L "$snapshot_record/value" ] || return 1
    fi
}

snapshot_path() {
    snapshot_root=$1
    label=$2
    source_path=$3
    snapshot_record=$snapshot_root/$label
    snapshot_new=$snapshot_root/.$label.$$.new
    rm -rf "$snapshot_new" || return 1
    mkdir -p "$snapshot_new" || return 1
    printf '%s\n' "$source_path" >"$snapshot_new/path" || return 1
    if [ -e "$source_path" ] || [ -L "$source_path" ]; then
        cp -a "$source_path" "$snapshot_new/value" || return 1
        : >"$snapshot_new/present" || return 1
    fi
    : >"$snapshot_new/complete" || return 1
    # The record must be self-contained on stable storage before its directory
    # name becomes authoritative.  A caller may mutate the live path as soon as
    # this helper returns.
    sync || return 1
    rm -rf "$snapshot_record" || return 1
    mv "$snapshot_new" "$snapshot_record" || return 1
    sync || return 1
    snapshot_record_is_complete "$snapshot_root" "$label" "$source_path"
}

restore_snapshot() {
    snapshot_root=$1
    label=$2
    record=$snapshot_root/$label
    if [ ! -e "$record" ] && [ ! -L "$record" ]; then
        return 0
    fi
    [ -d "$record" ] && [ ! -L "$record" ] && \
        [ -f "$record/path" ] && [ ! -L "$record/path" ] || {
            echo "deployment: refusing incomplete snapshot: $record" >&2
            return 1
        }
    target_path=$(sed -n '1p' "$record/path")
    snapshot_record_is_complete "$snapshot_root" "$label" "$target_path" || {
        echo "deployment: refusing incomplete snapshot: $record" >&2
        return 1
    }
    case "$target_path" in
        /usr/lib/systemd/system/*|/etc/systemd/system/*|/home/root/*|/tmp/*) ;;
        *)
            echo "deployment: refusing unsafe snapshot target: $target_path" >&2
            return 1
            ;;
    esac
    if ! canonical_absolute_path "$target_path"; then
            echo "deployment: refusing non-canonical snapshot target: $target_path" >&2
            return 1
    fi
    rm -rf "$target_path"
    if [ -e "$record/present" ]; then
        mkdir -p "$(dirname "$target_path")"
        cp -a "$record/value" "$target_path"
    fi
}

assert_aarch64_elf() {
    artifact=$1
    # reMarkable's BusyBox `od` omits GNU's -A/-N/-j options. `hexdump`
    # supports the same bounded byte reads on both the build host and device.
    magic=$(hexdump -v -n 4 -e '1/1 "%02x"' "$artifact" 2>/dev/null)
    machine=$(hexdump -v -s 18 -n 2 -e '1/1 "%02x"' "$artifact" 2>/dev/null)
    [ "$magic" = 7f454c46 ] && [ "$machine" = b700 ] || {
        echo "deployment: expected AArch64 ELF artifact: $artifact" >&2
        return 1
    }
}
