#!/bin/sh

# Power-loss recovery for the disposable real-device acceptance environment.
# Callers must first prove that the PID recorded by REMAGIC_TEST_LOCK is dead.

REMAGIC_ACCEPTANCE_FORMAT=remagic-acceptance-v1

remagic_acceptance_safe_path() {
    local acceptance_path acceptance_relative
    [ "$#" -eq 1 ] || return 1
    acceptance_path=$1
    case "$acceptance_path" in /|''|*[!A-Za-z0-9_./@+-]*) return 1 ;; /*) ;; *) return 1 ;; esac
    acceptance_relative=${acceptance_path#/}
    case "/$acceptance_relative/" in */../*|*/./*|*//*) return 1 ;; esac
}

remagic_acceptance_prepare_stage_path() {
    local acceptance_owner acceptance_parent acceptance_name
    [ "$#" -eq 1 ] || return 1
    acceptance_owner=$1
    case "$acceptance_owner" in ''|*[!0-9]*|0|1) return 1 ;; esac
    acceptance_parent=$(dirname "$REMAGIC_TEST_ROOT")
    acceptance_name=${REMAGIC_TEST_ROOT##*/}
    [ -n "$acceptance_name" ] || return 1
    printf '%s/.%s.remagic-prepare.%s\n' \
        "$acceptance_parent" "$acceptance_name" "$acceptance_owner"
}

remagic_acceptance_override_temp_path() {
    local acceptance_unit
    [ "$#" -eq 1 ] || return 1
    acceptance_unit=$1
    printf '%s.remagic-new\n' "$(remagic_acceptance_dropin_path "$acceptance_unit")"
}

remagic_acceptance_set_state() {
    local acceptance_state acceptance_txn acceptance_tmp acceptance_lock_tmp
    [ "$#" -eq 1 ] || return 1
    acceptance_state=$1
    acceptance_txn=$REMAGIC_TEST_ROOT/transaction
    acceptance_tmp=$acceptance_txn/.state.$$.new
    printf '%s\n' "$acceptance_state" >"$acceptance_tmp" || return 1
    mv -f "$acceptance_tmp" "$acceptance_txn/state" || return 1
    acceptance_lock_tmp=$REMAGIC_TEST_LOCK/.state.$$.new
    printf '%s\n' "$acceptance_state" >"$acceptance_lock_tmp" || return 1
    mv -f "$acceptance_lock_tmp" "$REMAGIC_TEST_LOCK/state" || return 1
    sync || return 1
}

remagic_acceptance_prepare() {
    local acceptance_label acceptance_owner acceptance_stage acceptance_lock_tmp
    [ "$#" -eq 2 ] || return 1
    acceptance_label=$1
    acceptance_owner=$2
    remagic_acceptance_safe_path "$REMAGIC_TEST_ROOT" || return 1
    remagic_acceptance_safe_path "$REMAGIC_TEST_SESSION_ROOT" || return 1
    if [ -e "$REMAGIC_TEST_ROOT" ] || [ -L "$REMAGIC_TEST_ROOT" ]; then
        echo "acceptance isolation: existing test transaction needs recovery" >&2
        return 1
    fi
    acceptance_stage=$(remagic_acceptance_prepare_stage_path "$acceptance_owner") || return 1
    remagic_acceptance_safe_path "$acceptance_stage" || return 1
    if [ -e "$acceptance_stage" ] || [ -L "$acceptance_stage" ]; then
        echo "acceptance isolation: stale journal preparation needs recovery" >&2
        return 1
    fi
    if ! (
        mkdir -p "$acceptance_stage/transaction" || exit 1
        printf '%s\n' "$REMAGIC_ACCEPTANCE_FORMAT" >"$acceptance_stage/transaction/format" || exit 1
        printf '%s\n' "$acceptance_owner" >"$acceptance_stage/transaction/owner-pid" || exit 1
        printf '%s\n' "$REMAGIC_TEST_SESSION_ROOT" >"$acceptance_stage/transaction/session-root" || exit 1
        printf '%s\n' prepared >"$acceptance_stage/transaction/state" || exit 1
        printf '%s\n' "$acceptance_label" >"$acceptance_stage/test-kind" || exit 1
        sync || exit 1
    ); then
        rm -rf "$acceptance_stage" || return 1
        return 1
    fi
    if ! mv "$acceptance_stage" "$REMAGIC_TEST_ROOT"; then
        rm -rf "$acceptance_stage" || true
        return 1
    fi
    REMAGIC_TEST_TRANSACTION_PREPARED=true
    acceptance_lock_tmp=$REMAGIC_TEST_LOCK/.state.$$.new
    printf '%s\n' prepared >"$acceptance_lock_tmp" || return 1
    mv -f "$acceptance_lock_tmp" "$REMAGIC_TEST_LOCK/state" || return 1
    sync || return 1
}

remagic_acceptance_validate() {
    local acceptance_owner acceptance_txn acceptance_state
    [ "$#" -eq 1 ] || return 1
    acceptance_owner=$1
    acceptance_txn=$REMAGIC_TEST_ROOT/transaction
    [ "$(sed -n '1p' "$REMAGIC_TEST_LOCK/format" 2>/dev/null || true)" = "$REMAGIC_ACCEPTANCE_FORMAT" ] || return 1
    [ "$(sed -n '1p' "$REMAGIC_TEST_LOCK/test-root" 2>/dev/null || true)" = "$REMAGIC_TEST_ROOT" ] || return 1
    [ "$(sed -n '1p' "$acceptance_txn/format" 2>/dev/null || true)" = "$REMAGIC_ACCEPTANCE_FORMAT" ] || return 1
    [ "$(sed -n '1p' "$acceptance_txn/owner-pid" 2>/dev/null || true)" = "$acceptance_owner" ] || return 1
    [ "$(sed -n '1p' "$acceptance_txn/session-root" 2>/dev/null || true)" = "$REMAGIC_TEST_SESSION_ROOT" ] || return 1
    remagic_acceptance_safe_path "$REMAGIC_TEST_ROOT" || return 1
    remagic_acceptance_safe_path "$REMAGIC_TEST_SESSION_ROOT" || return 1
    acceptance_state=$(sed -n '1p' "$acceptance_txn/state" 2>/dev/null || true)
    case "$acceptance_state" in
        prepared|sessions-saved|overrides-installing|overrides-installed|sessions-restored|finished|recovered) ;;
        *) return 1 ;;
    esac
}

remagic_acceptance_save_sessions() {
    local acceptance_snapshot acceptance_new
    acceptance_snapshot=$REMAGIC_TEST_ROOT/original-manager-sessions
    acceptance_new=$REMAGIC_TEST_ROOT/.original-manager-sessions.$$.new
    rm -rf "$acceptance_new" || return 1
    mkdir -p "$acceptance_new" || return 1
    if [ -e "$REMAGIC_TEST_SESSION_ROOT" ] || [ -L "$REMAGIC_TEST_SESSION_ROOT" ]; then
        cp -a "$REMAGIC_TEST_SESSION_ROOT" "$acceptance_new/value" || {
            rm -rf "$acceptance_new" || true
            return 1
        }
        : >"$acceptance_new/present" || {
            rm -rf "$acceptance_new" || true
            return 1
        }
    fi
    : >"$acceptance_new/complete" || {
        rm -rf "$acceptance_new" || true
        return 1
    }
    sync || {
        rm -rf "$acceptance_new" || true
        return 1
    }
    rm -rf "$acceptance_snapshot" || return 1
    mv "$acceptance_new" "$acceptance_snapshot" || return 1
    sync || return 1
    remagic_acceptance_set_state sessions-saved || return 1
    REMAGIC_TEST_SESSIONS_SAVED=true
}

remagic_acceptance_restore_sessions() {
    local acceptance_snapshot acceptance_parent acceptance_stage acceptance_trash
    local acceptance_had_current
    acceptance_snapshot=$REMAGIC_TEST_ROOT/original-manager-sessions
    [ -e "$acceptance_snapshot/complete" ] || {
        echo "acceptance recovery: manager-session snapshot is incomplete" >&2
        return 1
    }
    acceptance_parent=$(dirname "$REMAGIC_TEST_SESSION_ROOT")
    acceptance_stage=$acceptance_parent/.remagic-acceptance-restore.$$
    acceptance_trash=$acceptance_parent/.remagic-acceptance-discard.$$
    mkdir -p "$acceptance_parent" || return 1
    rm -rf "$acceptance_stage" "$acceptance_trash" || return 1
    if [ -e "$acceptance_snapshot/present" ]; then
        [ -e "$acceptance_snapshot/value" ] || [ -L "$acceptance_snapshot/value" ] || return 1
        cp -a "$acceptance_snapshot/value" "$acceptance_stage" || return 1
    fi
    acceptance_had_current=false
    if [ -e "$REMAGIC_TEST_SESSION_ROOT" ] || [ -L "$REMAGIC_TEST_SESSION_ROOT" ]; then
        mv "$REMAGIC_TEST_SESSION_ROOT" "$acceptance_trash" || return 1
        acceptance_had_current=true
    fi
    if [ -e "$acceptance_snapshot/present" ]; then
        mv "$acceptance_stage" "$REMAGIC_TEST_SESSION_ROOT" || {
            [ "$acceptance_had_current" = false ] || \
                mv "$acceptance_trash" "$REMAGIC_TEST_SESSION_ROOT" || true
            return 1
        }
    fi
    sync || return 1
    rm -rf "$acceptance_trash" || return 1
    remagic_acceptance_set_state sessions-restored || return 1
    REMAGIC_TEST_SESSIONS_SAVED=false
}

remagic_acceptance_dropin_path() {
    [ "$#" -eq 1 ] || return 1
    printf '%s/%s.d/90-remagic-acceptance.conf\n' "$REMAGIC_TEST_SYSTEMD_RUNTIME_ROOT" "$1"
}

remagic_acceptance_remove_owned_overrides() {
    local acceptance_expected acceptance_unit acceptance_dropin acceptance_temp acceptance_owned
    acceptance_expected="Environment=REMAGIC_MANIFEST_ROOT=$REMAGIC_TEST_ROOT/manifests"
    for acceptance_unit in remagicd.service 'remagic-app@.service'; do
        acceptance_dropin=$(remagic_acceptance_dropin_path "$acceptance_unit")
        acceptance_temp=$(remagic_acceptance_override_temp_path "$acceptance_unit")
        if [ -e "$acceptance_temp" ] || [ -L "$acceptance_temp" ]; then
            [ -f "$acceptance_temp" ] || [ -L "$acceptance_temp" ] || {
                echo "acceptance recovery: refusing unsafe temporary drop-in: $acceptance_temp" >&2
                return 1
            }
            rm -f "$acceptance_temp" || return 1
        fi
        if [ -e "$acceptance_dropin" ] || [ -L "$acceptance_dropin" ]; then
            case "$acceptance_unit" in
                remagicd.service)
                    acceptance_owned=$(printf '[Service]\n%s' "$acceptance_expected")
                    ;;
                *)
                    acceptance_owned=$(printf '[Service]\n%s\nIPAddressDeny=any' "$acceptance_expected")
                    ;;
            esac
            [ -f "$acceptance_dropin" ] && [ ! -L "$acceptance_dropin" ] && \
                [ "$(cat "$acceptance_dropin" 2>/dev/null || true)" = "$acceptance_owned" ] || {
                echo "acceptance recovery: refusing unowned drop-in: $acceptance_dropin" >&2
                return 1
            }
            rm -f "$acceptance_dropin" || return 1
        fi
        rmdir "$(dirname "$acceptance_dropin")" 2>/dev/null || true
    done
    "$REMAGIC_TEST_SYSTEMCTL" daemon-reload >/dev/null 2>&1 || return 1
    REMAGIC_TEST_OVERRIDES_INSTALLED=false
}

remagic_acceptance_stop_managed() {
    "$REMAGIC_TEST_CTL" system >/dev/null 2>&1 || true
    "$REMAGIC_TEST_SYSTEMCTL" stop \
        remagic-app@magicpaper.service remagic-app@koreader.service \
        remagic-home.service remagic-display-host.service remagicd.service >/dev/null 2>&1
}

remagic_acceptance_wait_active() {
    local acceptance_unit acceptance_attempts
    [ "$#" -ge 1 ] && [ "$#" -le 2 ] || return 1
    acceptance_unit=$1
    acceptance_attempts=${2:-120}
    case $acceptance_attempts in ''|*[!0-9]*|0) return 1 ;; esac
    while [ "$acceptance_attempts" -gt 0 ]; do
        [ "$($REMAGIC_TEST_SYSTEMCTL is-active "$acceptance_unit" 2>/dev/null || true)" = active ] && return 0
        sleep 0.1
        acceptance_attempts=$((acceptance_attempts - 1))
    done
    return 1
}

remagic_acceptance_wait_stock() {
    local acceptance_attempts acceptance_status
    acceptance_attempts=120
    while [ "$acceptance_attempts" -gt 0 ]; do
        acceptance_status=$($REMAGIC_TEST_CTL status 2>/dev/null || true)
        if printf '%s' "$acceptance_status" | grep -q '"domain": "system"' &&
            [ "$($REMAGIC_TEST_SYSTEMCTL is-active xochitl.service 2>/dev/null || true)" = active ]; then
            return 0
        fi
        sleep 0.1
        acceptance_attempts=$((acceptance_attempts - 1))
    done
    return 1
}

remagic_acceptance_unit_is_loaded() {
    local acceptance_unit acceptance_load_state
    [ "$#" -eq 1 ] || return 1
    acceptance_unit=$1
    acceptance_load_state=$(
        "$REMAGIC_TEST_SYSTEMCTL" show "$acceptance_unit" \
            --property=LoadState --value 2>/dev/null || true
    )
    [ "$acceptance_load_state" = loaded ]
}

remagic_acceptance_restore_stock() {
    "$REMAGIC_TEST_SYSTEMCTL" start xochitl.service >/dev/null 2>&1 || return 1
    if remagic_acceptance_unit_is_loaded paperweight.service; then
        "$REMAGIC_TEST_SYSTEMCTL" start paperweight.service >/dev/null 2>&1 || return 1
    fi
    "$REMAGIC_TEST_SYSTEMCTL" start remagicd.service >/dev/null 2>&1 || return 1
    remagic_acceptance_wait_active remagicd.service 120 || return 1
    remagic_acceptance_wait_stock || return 1
}

remagic_acceptance_remove_test_root() {
    [ "$#" -eq 0 ] || return 1
    remagic_acceptance_safe_path "$REMAGIC_TEST_ROOT" || return 1
    rm -rf "$REMAGIC_TEST_ROOT" || return 1
    sync || return 1
    [ ! -e "$REMAGIC_TEST_ROOT" ] && [ ! -L "$REMAGIC_TEST_ROOT" ]
}

remagic_acceptance_remove_lock() {
    local acceptance_owner acceptance_lock_parent acceptance_lock_name acceptance_tombstone
    [ "$#" -eq 1 ] || return 1
    acceptance_owner=$1
    [ "$(sed -n '1p' "$REMAGIC_TEST_LOCK/pid" 2>/dev/null || true)" = "$acceptance_owner" ] || return 1
    remagic_acceptance_safe_path "$REMAGIC_TEST_LOCK" || return 1
    acceptance_lock_parent=$(dirname "$REMAGIC_TEST_LOCK")
    acceptance_lock_name=${REMAGIC_TEST_LOCK##*/}
    acceptance_tombstone=$acceptance_lock_parent/.$acceptance_lock_name.remagic-released.$acceptance_owner.$$
    remagic_acceptance_safe_path "$acceptance_tombstone" || return 1
    [ ! -e "$acceptance_tombstone" ] && [ ! -L "$acceptance_tombstone" ] || return 1
    # Do not delete ownership evidence in place. The same-parent rename makes
    # the public lock either wholly present or wholly absent after a crash.
    mv "$REMAGIC_TEST_LOCK" "$acceptance_tombstone" || return 1
    sync || return 1
    if ! rm -rf "$acceptance_tombstone"; then
        echo "acceptance recovery: released lock tombstone retained at $acceptance_tombstone" >&2
        return 0
    fi
    sync || {
        echo "acceptance recovery: lock released but tombstone cleanup is not durable" >&2
        return 0
    }
}

remagic_acceptance_recover_orphan() {
    local acceptance_owner acceptance_lock_state acceptance_unit acceptance_dropin
    local acceptance_temp acceptance_stage acceptance_state
    [ "$#" -eq 1 ] || return 1
    acceptance_owner=$1
    acceptance_lock_state=$(sed -n '1p' "$REMAGIC_TEST_LOCK/state" 2>/dev/null || true)
    [ "$(sed -n '1p' "$REMAGIC_TEST_LOCK/format" 2>/dev/null || true)" = "$REMAGIC_ACCEPTANCE_FORMAT" ] || return 1
    [ "$(sed -n '1p' "$REMAGIC_TEST_LOCK/test-root" 2>/dev/null || true)" = "$REMAGIC_TEST_ROOT" ] || return 1
    [ "$(sed -n '1p' "$REMAGIC_TEST_LOCK/pid" 2>/dev/null || true)" = "$acceptance_owner" ] || return 1
    # Terminal state is published and synced only after sessions, overrides,
    # stock services, and diagnostics are settled. It remains authoritative
    # even if recursive root deletion was interrupted halfway through.
    case "$acceptance_lock_state" in
        finished|recovered)
            remagic_acceptance_remove_owned_overrides || return 1
            remagic_acceptance_restore_stock || return 1
            remagic_acceptance_remove_test_root || return 1
            remagic_acceptance_remove_lock "$acceptance_owner"
            return
            ;;
    esac
    if [ ! -e "$REMAGIC_TEST_ROOT/transaction" ]; then
        case "$acceptance_lock_state" in
            claimed)
                [ ! -e "$REMAGIC_TEST_ROOT" ] && [ ! -L "$REMAGIC_TEST_ROOT" ] || return 1
                for acceptance_unit in remagicd.service 'remagic-app@.service'; do
                    acceptance_dropin=$(remagic_acceptance_dropin_path "$acceptance_unit")
                    acceptance_temp=$(remagic_acceptance_override_temp_path "$acceptance_unit")
                    [ ! -e "$acceptance_dropin" ] && [ ! -L "$acceptance_dropin" ] || return 1
                    [ ! -e "$acceptance_temp" ] && [ ! -L "$acceptance_temp" ] || return 1
                done
                acceptance_stage=$(remagic_acceptance_prepare_stage_path "$acceptance_owner") || return 1
                remagic_acceptance_safe_path "$acceptance_stage" || return 1
                rm -rf "$acceptance_stage" || return 1
                remagic_acceptance_remove_lock "$acceptance_owner"
                return
                ;;
            *) return 1 ;;
        esac
    fi
    remagic_acceptance_validate "$acceptance_owner" || {
        echo "acceptance recovery: lock and journal ownership do not match" >&2
        return 1
    }
    acceptance_state=$(sed -n '1p' "$REMAGIC_TEST_ROOT/transaction/state")
    remagic_acceptance_stop_managed || return 1
    case "$acceptance_state" in
        prepared)
            for acceptance_unit in remagicd.service 'remagic-app@.service'; do
                acceptance_dropin=$(remagic_acceptance_dropin_path "$acceptance_unit")
                [ ! -e "$acceptance_dropin" ] && [ ! -L "$acceptance_dropin" ] || return 1
            done
            ;;
        *) remagic_acceptance_restore_sessions || return 1 ;;
    esac
    remagic_acceptance_remove_owned_overrides || return 1
    remagic_acceptance_restore_stock || return 1
    if [ -e "$REMAGIC_TEST_ROOT/transaction/agent-was-active" ]; then
        "$REMAGIC_TEST_SYSTEMCTL" start magicpaper-agent.service >/dev/null 2>&1 || return 1
    fi
    remagic_acceptance_set_state recovered || return 1
    # Keep the terminal state in the lock until removal of the transaction
    # root is durable. A crash between these two steps is then recoverable via
    # the no-journal `recovered` case above.
    remagic_acceptance_remove_test_root || return 1
    remagic_acceptance_remove_lock "$acceptance_owner"
}
