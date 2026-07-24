#!/bin/sh

# Disposable production-shaped environment for real-device acceptance tests.
# Call remagic_test_begin before entering the managed domain and call
# remagic_test_finish from the caller's EXIT trap. The test uses the real
# binaries, display host and systemd units, but never production application
# data or network credentials.

REMAGIC_TEST_LOCK=${REMAGIC_TEST_LOCK:-/run/remagic-acceptance.lock}
REMAGIC_INSTALL_LOCK=${REMAGIC_INSTALL_LOCK:-/run/remagic-install.lock}
REMAGIC_TEST_ROOT=${REMAGIC_TEST_ROOT:-/home/root/.local/state/remagic/acceptance/current}
REMAGIC_APP_ROOT=${REMAGIC_APP_ROOT:-/home/root/apps/remagic}
REMAGIC_TEST_TEMPLATES=${REMAGIC_TEST_TEMPLATES:-$REMAGIC_APP_ROOT/share/testing/manifests}
REMAGIC_TEST_PRODUCTION_MANIFEST_ROOT=${REMAGIC_TEST_PRODUCTION_MANIFEST_ROOT:-/home/root/.local/share/remagic/apps.d}
REMAGIC_TEST_APPS_ROOT=${REMAGIC_TEST_APPS_ROOT:-/home/root/apps}
REMAGIC_TEST_CTL=${REMAGIC_TEST_CTL:-$REMAGIC_APP_ROOT/bin/remagicctl}
REMAGIC_TEST_SYSTEMCTL=${REMAGIC_TEST_SYSTEMCTL:-systemctl}
REMAGIC_TEST_SYSTEMD_RUNTIME_ROOT=${REMAGIC_TEST_SYSTEMD_RUNTIME_ROOT:-/run/systemd/system}
REMAGIC_TEST_SESSION_ROOT=${REMAGIC_TEST_SESSION_ROOT:-/home/root/.local/state/remagic/sessions}
REMAGIC_TEST_DIAGNOSTICS_ROOT=${REMAGIC_TEST_DIAGNOSTICS_ROOT:-$(dirname "$REMAGIC_TEST_ROOT")/diagnostics}
# Hash mutable data; release/install gates already verify content-addressed KOReader payload bytes.
REMAGIC_TEST_PROTECTED_PATHS=${REMAGIC_TEST_PROTECTED_PATHS:-/home/root/riddle-data:/home/root/.config/riddle:/home/root/.local/share/remagic-magicpaper:/home/root/.config/remagic-magicpaper:/home/root/.local/share/magicpaper:/home/root/.config/magicpaper:/home/root/.local/state/magicpaper:/home/root/.cache/magicpaper:/home/root/apps/koreader/current:/home/root/.local/share/remagic/apps.d/koreader.toml:/home/root/.local/state/remagic/packages/koreader.json:/home/root/.local/share/remagic-koreader:/home/root/.local/share/koreader-for-remagic:/home/root/.local/state/koreader-for-remagic}
REMAGIC_TEST_RECOVERY_HELPER=${REMAGIC_TEST_RECOVERY_HELPER:-/home/root/apps/remagic/libexec/device-test-recovery.sh}
REMAGIC_TEST_MANIFEST_HELPER=${REMAGIC_TEST_MANIFEST_HELPER:-/home/root/apps/remagic/libexec/device-test-manifests.sh}

[ -r "$REMAGIC_TEST_RECOVERY_HELPER" ] || {
    echo "acceptance isolation: recovery helper is missing" >&2
    return 1 2>/dev/null || exit 1
}
. "$REMAGIC_TEST_RECOVERY_HELPER"
[ -r "$REMAGIC_TEST_MANIFEST_HELPER" ] || {
    echo "acceptance isolation: manifest helper is missing" >&2
    return 1 2>/dev/null || exit 1
}
. "$REMAGIC_TEST_MANIFEST_HELPER"

REMAGIC_TEST_BEGUN=false
REMAGIC_TEST_TRANSACTION_PREPARED=false
REMAGIC_TEST_LOCKED=false
REMAGIC_TEST_OVERRIDES_INSTALLED=false
REMAGIC_TEST_SESSIONS_SAVED=false
REMAGIC_TEST_AGENT_WAS_ACTIVE=false
REMAGIC_TEST_PRODUCTION_FINGERPRINT=
REMAGIC_TEST_RECOVERY_GUARD=false
REMAGIC_TEST_FOREIGN_OVERRIDE=false
REMAGIC_TEST_U64_MAX=18446744073709551615

remagic_test_u64_canonical() {
    local value value_length
    [ "$#" -eq 1 ] || return 1
    value=$1
    case $value in ''|*[!0-9]*) return 1 ;; esac
    while [ "${value#0}" != "$value" ]; do
        value=${value#0}
    done
    [ -n "$value" ] || value=0
    value_length=${#value}
    [ "$value_length" -le 20 ] || return 1
    if [ "$value_length" -eq 20 ] && \
        LC_ALL=C [ "$value" \> "$REMAGIC_TEST_U64_MAX" ]; then
        return 1
    fi
    printf '%s\n' "$value"
}

remagic_test_u64_nonzero() {
    local value
    [ "$#" -eq 1 ] || return 1
    value=$(remagic_test_u64_canonical "$1") || return 1
    [ "$value" != 0 ]
}

remagic_test_u64_equal() {
    local left right
    [ "$#" -eq 2 ] || return 1
    left=$(remagic_test_u64_canonical "$1") || return 1
    right=$(remagic_test_u64_canonical "$2") || return 1
    [ "$left" = "$right" ]
}

remagic_test_u64_greater() {
    local left right left_length right_length
    [ "$#" -eq 2 ] || return 1
    left=$(remagic_test_u64_canonical "$1") || return 1
    right=$(remagic_test_u64_canonical "$2") || return 1
    left_length=${#left}
    right_length=${#right}
    [ "$left_length" -gt "$right_length" ] && return 0
    [ "$left_length" -eq "$right_length" ] || return 1
    LC_ALL=C [ "$left" \> "$right" ]
}

remagic_test_u64_next_value() {
    local value prefix digit result carry
    [ "$#" -eq 1 ] || return 1
    value=$(remagic_test_u64_canonical "$1") || return 1
    [ "$value" != "$REMAGIC_TEST_U64_MAX" ] || return 1
    result=
    carry=1
    while [ -n "$value" ]; do
        if [ "$carry" -eq 0 ]; then
            result=$value$result
            break
        fi
        prefix=${value%?}
        digit=${value#$prefix}
        digit=$((digit + carry))
        if [ "$digit" -eq 10 ]; then
            digit=0
        else
            carry=0
        fi
        result=$digit$result
        value=$prefix
    done
    [ "$carry" -eq 0 ] || result=1$result
    printf '%s\n' "$result"
}

remagic_test_u64_is_next() {
    local expected actual
    [ "$#" -eq 2 ] || return 1
    expected=$(remagic_test_u64_next_value "$1") || return 1
    actual=$(remagic_test_u64_canonical "$2") || return 1
    [ "$expected" = "$actual" ]
}

remagic_test_acquire_recovery_guard() {
    mkdir "$REMAGIC_INSTALL_LOCK" 2>/dev/null || {
        echo "acceptance isolation: install won the orphan-recovery race" >&2
        return 1
    }
    if ! printf '%s\n' "$$" >"$REMAGIC_INSTALL_LOCK/pid" ||
        ! printf '%s\n' acceptance-recovery >"$REMAGIC_INSTALL_LOCK/owner" ||
        ! sync; then
        rm -f "$REMAGIC_INSTALL_LOCK/pid" "$REMAGIC_INSTALL_LOCK/owner" || true
        rmdir "$REMAGIC_INSTALL_LOCK" 2>/dev/null || true
        return 1
    fi
    REMAGIC_TEST_RECOVERY_GUARD=true
}

remagic_test_release_recovery_guard() {
    local guard_owner
    [ "$REMAGIC_TEST_RECOVERY_GUARD" = true ] || return 0
    guard_owner=$(sed -n '1p' "$REMAGIC_INSTALL_LOCK/pid" 2>/dev/null || true)
    if [ "$guard_owner" = "$$" ]; then
        rm -f "$REMAGIC_INSTALL_LOCK/pid" "$REMAGIC_INSTALL_LOCK/owner" || return 1
        rmdir "$REMAGIC_INSTALL_LOCK" 2>/dev/null || return 1
        sync || return 1
    else
        return 1
    fi
    REMAGIC_TEST_RECOVERY_GUARD=false
}

remagic_test_wait_active() {
    local unit attempts
    [ "$#" -ge 1 ] && [ "$#" -le 2 ] || return 1
    unit=$1
    attempts=${2:-120}
    case $attempts in ''|*[!0-9]*|0) return 1 ;; esac
    while [ "$attempts" -gt 0 ]; do
        [ "$($REMAGIC_TEST_SYSTEMCTL is-active "$unit" 2>/dev/null || true)" = active ] && return 0
        sleep 0.1
        attempts=$((attempts - 1))
    done
    return 1
}

remagic_test_wait_stock() {
    local attempts status
    attempts=120
    while [ "$attempts" -gt 0 ]; do
        status=$($REMAGIC_TEST_CTL status 2>/dev/null || true)
        if printf '%s' "$status" | grep -q '"domain": "system"' &&
            [ "$($REMAGIC_TEST_SYSTEMCTL is-active xochitl.service 2>/dev/null || true)" = active ]; then
            return 0
        fi
        sleep 0.1
        attempts=$((attempts - 1))
    done
    return 1
}

remagic_test_recover_lockless_journal() {
    local owner state
    [ ! -e "$REMAGIC_TEST_LOCK" ] || return 0
    [ -e "$REMAGIC_TEST_ROOT" ] || [ -L "$REMAGIC_TEST_ROOT" ] || return 0
    [ -d "$REMAGIC_TEST_ROOT" ] && [ ! -L "$REMAGIC_TEST_ROOT" ] || {
        echo "acceptance isolation: unsafe lockless test root" >&2
        return 1
    }
    owner=$(sed -n '1p' "$REMAGIC_TEST_ROOT/transaction/owner-pid" 2>/dev/null || true)
    state=$(sed -n '1p' "$REMAGIC_TEST_ROOT/transaction/state" 2>/dev/null || true)
    case $owner in
        ''|*[!0-9]*|0|1)
            echo "acceptance isolation: lockless journal has no provable owner" >&2
            return 1
            ;;
        *) kill -0 "$owner" 2>/dev/null && {
            echo "acceptance isolation: lockless journal owner is still alive" >&2
            return 1
        } ;;
    esac
    case $state in
        prepared|sessions-saved|overrides-installing|overrides-installed|sessions-restored|finished|recovered) ;;
        *) echo "acceptance isolation: lockless journal has an invalid state" >&2; return 1 ;;
    esac
    remagic_test_acquire_recovery_guard || return 1
    if ! mkdir "$REMAGIC_TEST_LOCK" 2>/dev/null; then
        remagic_test_release_recovery_guard || true
        echo "acceptance isolation: another test won lockless recovery" >&2
        return 1
    fi
    if ! printf '%s\n' "$owner" >"$REMAGIC_TEST_LOCK/pid" ||
        ! printf '%s\n' "$REMAGIC_ACCEPTANCE_FORMAT" >"$REMAGIC_TEST_LOCK/format" ||
        ! printf '%s\n' "$REMAGIC_TEST_ROOT" >"$REMAGIC_TEST_LOCK/test-root" ||
        ! printf '%s\n' "$state" >"$REMAGIC_TEST_LOCK/state" || ! sync; then
        remagic_test_release_recovery_guard || true
        return 1
    fi
    remagic_acceptance_recover_orphan "$owner" || {
        remagic_test_release_recovery_guard || true
        echo "acceptance isolation: lockless transaction recovery failed" >&2
        return 1
    }
    remagic_test_release_recovery_guard
}

remagic_test_lock() {
    local owner live
    if [ -e "$REMAGIC_INSTALL_LOCK" ]; then
        echo "acceptance isolation: an install transaction is present" >&2
        return 1
    fi
    remagic_test_recover_lockless_journal || return 1
    if mkdir "$REMAGIC_TEST_LOCK" 2>/dev/null; then
        if ! printf '%s\n' "$$" >"$REMAGIC_TEST_LOCK/pid" ||
            ! printf '%s\n' "$REMAGIC_ACCEPTANCE_FORMAT" >"$REMAGIC_TEST_LOCK/format" ||
            ! printf '%s\n' "$REMAGIC_TEST_ROOT" >"$REMAGIC_TEST_LOCK/test-root" ||
            ! printf '%s\n' claimed >"$REMAGIC_TEST_LOCK/state" ||
            ! sync; then
            rm -f "$REMAGIC_TEST_LOCK/pid" "$REMAGIC_TEST_LOCK/format" \
                "$REMAGIC_TEST_LOCK/test-root" "$REMAGIC_TEST_LOCK/state" || true
            rmdir "$REMAGIC_TEST_LOCK" 2>/dev/null || true
            return 1
        fi
        REMAGIC_TEST_LOCKED=true
        # Close the install-vs-acceptance TOCTOU: an installer that acquired
        # its lock after our first check must win before either side mutates
        # services or manifests.
        if [ -e "$REMAGIC_INSTALL_LOCK" ]; then
            remagic_test_release_lock || return 1
            echo "acceptance isolation: install transaction won the lock race" >&2
            return 1
        fi
        return 0
    fi
    owner=$(sed -n '1p' "$REMAGIC_TEST_LOCK/pid" 2>/dev/null || true)
    case $owner in
        ''|*[!0-9]*|0|1)
            echo "acceptance isolation: stale lock has no provable owner" >&2
            return 1
            ;;
        *) if kill -0 "$owner" 2>/dev/null; then live=true; else live=false; fi ;;
    esac
    if [ "$live" = false ]; then
        remagic_test_acquire_recovery_guard || return 1
        remagic_acceptance_recover_orphan "$owner" || {
            remagic_test_release_recovery_guard || true
            echo "acceptance isolation: interrupted transaction recovery failed" >&2
            return 1
        }
        remagic_test_release_recovery_guard || return 1
        mkdir "$REMAGIC_TEST_LOCK" || return 1
        if ! printf '%s\n' "$$" >"$REMAGIC_TEST_LOCK/pid" ||
            ! printf '%s\n' "$REMAGIC_ACCEPTANCE_FORMAT" >"$REMAGIC_TEST_LOCK/format" ||
            ! printf '%s\n' "$REMAGIC_TEST_ROOT" >"$REMAGIC_TEST_LOCK/test-root" ||
            ! printf '%s\n' claimed >"$REMAGIC_TEST_LOCK/state" ||
            ! sync; then
            rm -f "$REMAGIC_TEST_LOCK/pid" "$REMAGIC_TEST_LOCK/format" \
                "$REMAGIC_TEST_LOCK/test-root" "$REMAGIC_TEST_LOCK/state" || true
            rmdir "$REMAGIC_TEST_LOCK" 2>/dev/null || true
            return 1
        fi
        REMAGIC_TEST_LOCKED=true
        if [ -e "$REMAGIC_INSTALL_LOCK" ]; then
            remagic_test_release_lock || return 1
            echo "acceptance isolation: install transaction won the lock race" >&2
            return 1
        fi
        return 0
    fi
    echo "acceptance isolation: another test is active (pid=$owner)" >&2
    return 1
}

remagic_test_release_lock() {
    local owner
    [ "$REMAGIC_TEST_LOCKED" = true ] || return 0
    owner=$(sed -n '1p' "$REMAGIC_TEST_LOCK/pid" 2>/dev/null || true)
    if [ "$owner" = "$$" ]; then
        remagic_acceptance_remove_lock "$owner" || return 1
    else
        return 1
    fi
    REMAGIC_TEST_LOCKED=false
}

remagic_test_protected_paths() {
    local path IFS
    IFS=:
    for path in $REMAGIC_TEST_PROTECTED_PATHS; do
        if [ -e "$path" ] || [ -L "$path" ]; then
            printf '%s\n' "$path" || return 1
        else
            printf '!missing\t%s\n' "$path" || return 1
        fi
    done
}

remagic_test_fingerprint() {
    local roots listing sorted details root path metadata digest_line digest link_target final_line final_digest
    roots=$REMAGIC_TEST_ROOT/.protected-roots.$$
    listing=$REMAGIC_TEST_ROOT/.protected-files.$$
    sorted=$REMAGIC_TEST_ROOT/.protected-files.$$.sorted
    details=$REMAGIC_TEST_ROOT/.protected-details.$$
    : >"$roots" || return 1
    : >"$listing" || { rm -f "$roots" || true; return 1; }
    : >"$details" || { rm -f "$roots" "$listing" || true; return 1; }
    remagic_test_protected_paths >"$roots" || {
        rm -f "$roots" "$listing" "$details" || true
        return 1
    }
    while IFS= read -r root; do
        case $root in
            '!missing'*) printf '%s\n' "$root" >>"$listing" || {
                rm -f "$roots" "$listing" "$details" || true
                return 1
            } ;;
            *) remagic_acceptance_fs_command find \
                "$root" -xdev \( -type d -o -type f -o -type l \) -print \
                >>"$listing" || {
                    rm -f "$roots" "$listing" "$details" || true
                    return 1
                } ;;
        esac
    done <"$roots"
    LC_ALL=C sort "$listing" >"$sorted" || {
        rm -f "$roots" "$listing" "$sorted" "$details" || true
        return 1
    }
    mv "$sorted" "$listing" || {
        rm -f "$roots" "$listing" "$sorted" "$details" || true
        return 1
    }

    while IFS= read -r path; do
        case $path in
            '!missing'*) printf '%s\n' "$path" >>"$details" || {
                rm -f "$roots" "$listing" "$details" || true
                return 1
            } ;;
            *)
                metadata=$(stat -c '%F\t%a\t%u\t%g\t%s\t%Y' "$path" 2>/dev/null) || {
                    rm -f "$roots" "$listing" "$details" || true
                    return 1
                }
                if [ -L "$path" ]; then
                    link_target=$(readlink "$path") || {
                        rm -f "$roots" "$listing" "$details" || true
                        return 1
                    }
                    printf 'L\t%s\t%s\t%s\n' "$path" "$metadata" "$link_target" \
                        >>"$details" || {
                            rm -f "$roots" "$listing" "$details" || true
                            return 1
                        }
                elif [ -f "$path" ]; then
                    digest_line=$(remagic_acceptance_fs_command sha256sum "$path") || {
                        rm -f "$roots" "$listing" "$details" || true
                        return 1
                    }
                    digest=${digest_line%% *}
                    [ "${#digest}" -eq 64 ] || {
                        rm -f "$roots" "$listing" "$details" || true
                        return 1
                    }
                    case $digest in *[!0-9A-Fa-f]*)
                        rm -f "$roots" "$listing" "$details" || true
                        return 1
                    esac
                    printf 'F\t%s\t%s\t%s\n' "$path" "$metadata" "$digest" \
                        >>"$details" || {
                            rm -f "$roots" "$listing" "$details" || true
                            return 1
                        }
                else
                    printf 'D\t%s\t%s\n' "$path" "$metadata" >>"$details" || {
                        rm -f "$roots" "$listing" "$details" || true
                        return 1
                    }
                fi
                ;;
        esac
    done <"$listing"
    final_line=$(remagic_acceptance_fs_command sha256sum "$details") || {
        rm -f "$roots" "$listing" "$details" || true
        return 1
    }
    final_digest=${final_line%% *}
    [ "${#final_digest}" -eq 64 ] || {
        rm -f "$roots" "$listing" "$details" || true
        return 1
    }
    case $final_digest in *[!0-9A-Fa-f]*)
        rm -f "$roots" "$listing" "$details" || true
        return 1
    esac
    rm -f "$roots" "$listing" "$details" || return 1
    printf '%s\n' "$final_digest"
}

remagic_test_assert_override_slots() {
    local unit dropin dropin_temp
    for unit in remagicd.service remagic-home.service 'remagic-app@.service'; do
        dropin=$REMAGIC_TEST_SYSTEMD_RUNTIME_ROOT/$unit.d/90-remagic-acceptance.conf
        dropin_temp=$(remagic_acceptance_override_temp_path "$unit")
        if [ -e "$dropin" ] || [ -L "$dropin" ] || \
           [ -e "$dropin_temp" ] || [ -L "$dropin_temp" ]; then
            [ "$REMAGIC_TEST_TRANSACTION_PREPARED" = false ] || REMAGIC_TEST_FOREIGN_OVERRIDE=true
            echo "acceptance isolation: reserved drop-in already exists: $dropin" >&2
            return 1
        fi
    done
}

remagic_test_publish_override() {
    local unit manifest_root dropin dropin_temp dropin_dir
    [ "$#" -eq 2 ] || return 1
    unit=$1
    manifest_root=$2
    dropin=$(remagic_acceptance_dropin_path "$unit")
    dropin_temp=$(remagic_acceptance_override_temp_path "$unit")
    dropin_dir=$(dirname "$dropin")
    mkdir -p "$dropin_dir" || return 1
    if [ -e "$dropin" ] || [ -L "$dropin" ] || \
       [ -e "$dropin_temp" ] || [ -L "$dropin_temp" ]; then
        echo "acceptance isolation: reserved drop-in slot changed during publication: $dropin" >&2
        return 1
    fi
    if ! (
        set -C
        umask 077
        case $unit in
            'remagic-app@.service')
                printf '%s\n' '[Service]' \
                    "Environment=REMAGIC_MANIFEST_ROOT=$manifest_root" \
                    'IPAddressDeny=any' >"$dropin_temp" || exit 1
                ;;
            remagic-home.service)
                printf '%s\n' '[Service]' \
                    "Environment=REMAGIC_WELCOME_MARKER=$REMAGIC_TEST_ROOT/welcome-v1" \
                    >"$dropin_temp" || exit 1
                ;;
            *)
                printf '%s\n' '[Service]' \
                    "Environment=REMAGIC_MANIFEST_ROOT=$manifest_root" >"$dropin_temp" || exit 1
                ;;
        esac
        chmod 0644 "$dropin_temp" || exit 1
    ); then
        return 1
    fi
    sync || return 1
    mv "$dropin_temp" "$dropin" || return 1
    sync || return 1
}

remagic_test_install_overrides() {
    local manifest_root unit network_deny
    manifest_root=$REMAGIC_TEST_ROOT/manifests
    case $manifest_root in *[!A-Za-z0-9_./-]*) return 1 ;; esac
    mkdir -p "$manifest_root" || return 1
    remagic_test_materialize_manifests "$manifest_root" || return 1

    remagic_test_assert_override_slots || return 1

    # Set before the first drop-in write so caller cleanup removes a partial
    # override if storage or daemon-reload fails mid-transaction.
    REMAGIC_TEST_OVERRIDES_INSTALLED=true
    for unit in remagicd.service remagic-home.service 'remagic-app@.service'; do
        remagic_test_publish_override "$unit" "$manifest_root" || return 1
    done
    "$REMAGIC_TEST_SYSTEMCTL" daemon-reload || return 1
    network_deny=$($REMAGIC_TEST_SYSTEMCTL show remagic-app@magicpaper.service \
        --property=IPAddressDeny --value 2>/dev/null || true)
    case "$network_deny" in
        any|*0.0.0.0/0*::/0*|*::/0*0.0.0.0/0*) ;;
        *)
            echo "acceptance isolation: systemd IPAddressDeny=any is unavailable" >&2
            return 1
            ;;
    esac
}

remagic_test_remove_overrides() {
    remagic_acceptance_remove_owned_overrides
}

remagic_test_seed_data() {
    mkdir -p \
        "$REMAGIC_TEST_ROOT/magicpaper/config" \
        "$REMAGIC_TEST_ROOT/magicpaper/data" \
        "$REMAGIC_TEST_ROOT/magicpaper/empty-library" \
        "$REMAGIC_TEST_ROOT/magicpaper/state" \
        "$REMAGIC_TEST_ROOT/magicpaper/cache" \
        "$REMAGIC_TEST_ROOT/koreader/data/settings" \
        "$REMAGIC_TEST_ROOT/koreader/config" \
        "$REMAGIC_TEST_ROOT/koreader/state" \
        "$REMAGIC_TEST_ROOT/koreader/cache" \
        "$REMAGIC_TEST_ROOT/koreader/library-state" \
        "$REMAGIC_TEST_ROOT/koreader/source-library" \
        "$REMAGIC_TEST_ROOT/koreader/local-books" \
        "$REMAGIC_TEST_ROOT/koreader/runtime" \
        "$REMAGIC_TEST_ROOT/koreader/empty-legacy" || return 1
    : >"$REMAGIC_TEST_ROOT/magicpaper/config/oracle.env" || return 1
    cat >"$REMAGIC_TEST_ROOT/koreader/data/settings.reader.lua" <<'EOF' || return 1
return {
    ["language"] = "zh_CN",
}
EOF
    printf '%s\n' completed >"$REMAGIC_TEST_ROOT/welcome-v1" || return 1
    chmod -R go-rwx "$REMAGIC_TEST_ROOT" || return 1
}

remagic_test_begin() {
    local label manifest unit
    label=${1:-device}
    [ "$#" -le 1 ] || return 1
    [ "$(id -u)" -eq 0 ] || [ "${REMAGIC_TEST_ALLOW_NON_ROOT:-0}" = 1 ] || {
        echo "acceptance isolation must run as root" >&2
        return 1
    }
    [ -x "$REMAGIC_TEST_CTL" ] || {
        echo "acceptance isolation: remagicctl is missing" >&2
        return 1
    }
    for manifest in magicpaper koreader; do
        [ -s "$REMAGIC_TEST_TEMPLATES/$manifest.toml" ] || {
            echo "acceptance isolation: missing test manifest $manifest" >&2
            return 1
        }
    done
    # Refuse an already occupied reserved slot before claiming any state. The
    # check is repeated under the lock below to close the publication race.
    remagic_test_assert_override_slots || return 1
    remagic_test_lock || return 1
    # Publish the recoverable journal immediately after the lock. From this
    # point every service, session and override mutation has a durable owner.
    remagic_acceptance_prepare "$label" "$$" || return 1
    REMAGIC_TEST_TRANSACTION_PREPARED=true
    remagic_test_wait_stock || {
        echo "acceptance isolation: tests require an idle stock-domain device" >&2
        return 1
    }
    for unit in remagic-app@magicpaper.service remagic-app@koreader.service; do
        [ "$($REMAGIC_TEST_SYSTEMCTL is-active "$unit" 2>/dev/null || true)" != active ] || {
            echo "acceptance isolation: production application is still active: $unit" >&2
            return 1
        }
    done

    remagic_test_assert_override_slots || return 1
    remagic_test_seed_data || return 1
    if [ "$($REMAGIC_TEST_SYSTEMCTL is-active magicpaper-agent.service 2>/dev/null || true)" = active ]; then
        REMAGIC_TEST_AGENT_WAS_ACTIVE=true
        : >"$REMAGIC_TEST_ROOT/transaction/agent-was-active" || return 1
        sync || return 1
        "$REMAGIC_TEST_SYSTEMCTL" stop magicpaper-agent.service || return 1
    fi
    "$REMAGIC_TEST_SYSTEMCTL" stop remagicd.service || return 1
    remagic_acceptance_save_sessions || return 1
    REMAGIC_TEST_PRODUCTION_FINGERPRINT=$(remagic_test_fingerprint) || return 1
    printf '%s\n' "$REMAGIC_TEST_PRODUCTION_FINGERPRINT" \
        >"$REMAGIC_TEST_ROOT/production.before.sha256" || return 1

    remagic_acceptance_set_state overrides-installing || return 1
    remagic_test_install_overrides || return 1
    remagic_acceptance_set_state overrides-installed || return 1
    "$REMAGIC_TEST_SYSTEMCTL" restart remagicd.service || return 1
    remagic_test_wait_active remagicd.service 120 || return 1
    remagic_test_wait_stock || return 1
    REMAGIC_TEST_BEGUN=true
}

remagic_test_finish() {
    local finish_status infrastructure_restored after diagnostic_root
    finish_status=0
    infrastructure_restored=true
    if [ "$REMAGIC_TEST_TRANSACTION_PREPARED" = true ]; then
        "$REMAGIC_TEST_CTL" system >/dev/null 2>&1 || true
        "$REMAGIC_TEST_SYSTEMCTL" stop remagic-app@magicpaper.service remagic-app@koreader.service \
            remagic-home.service remagic-display-host.service remagicd.service >/dev/null 2>&1 || true
        if [ "$REMAGIC_TEST_OVERRIDES_INSTALLED" = true ]; then
            remagic_test_remove_overrides || infrastructure_restored=false
        fi
        if [ "$REMAGIC_TEST_SESSIONS_SAVED" = true ]; then
            remagic_acceptance_restore_sessions || infrastructure_restored=false
        fi
        [ "$REMAGIC_TEST_FOREIGN_OVERRIDE" = false ] || infrastructure_restored=false
        if [ "$infrastructure_restored" = true ]; then
            "$REMAGIC_TEST_SYSTEMCTL" start remagicd.service >/dev/null 2>&1 || infrastructure_restored=false
            remagic_test_wait_active remagicd.service 120 || infrastructure_restored=false
            remagic_test_wait_stock || infrastructure_restored=false
        fi
    fi

    if [ -n "$REMAGIC_TEST_PRODUCTION_FINGERPRINT" ]; then
        after=$(remagic_test_fingerprint) || after=unreadable
        printf '%s\n' "$after" >"$REMAGIC_TEST_ROOT/production.after.sha256" || {
            infrastructure_restored=false
            finish_status=1
        }
        if [ "$after" != "$REMAGIC_TEST_PRODUCTION_FINGERPRINT" ]; then
            echo "acceptance isolation: protected production data changed" >&2
            echo "before=$REMAGIC_TEST_PRODUCTION_FINGERPRINT after=$after" >&2
            finish_status=1
        fi
    fi
    if [ "$infrastructure_restored" = true ] && [ "$REMAGIC_TEST_AGENT_WAS_ACTIVE" = true ]; then
        "$REMAGIC_TEST_SYSTEMCTL" start magicpaper-agent.service >/dev/null 2>&1 || infrastructure_restored=false
    fi
    if [ "$infrastructure_restored" = true ] && [ "$REMAGIC_TEST_TRANSACTION_PREPARED" = true ]; then
        remagic_acceptance_set_state finished || infrastructure_restored=false
    fi
    if [ "$infrastructure_restored" = true ] && [ "$finish_status" -eq 0 ]; then
        if [ "$REMAGIC_TEST_TRANSACTION_PREPARED" = true ]; then
            remagic_acceptance_remove_test_root || infrastructure_restored=false
        fi
    elif [ "$infrastructure_restored" = true ] && [ "$REMAGIC_TEST_TRANSACTION_PREPARED" = true ]; then
        remagic_acceptance_safe_path "$REMAGIC_TEST_DIAGNOSTICS_ROOT" || infrastructure_restored=false
        [ "$infrastructure_restored" = false ] || \
            mkdir -p "$REMAGIC_TEST_DIAGNOSTICS_ROOT" || infrastructure_restored=false
        diagnostic_root=$REMAGIC_TEST_DIAGNOSTICS_ROOT/$(date +%Y%m%d-%H%M%S 2>/dev/null || echo unknown)-$$
        if [ "$infrastructure_restored" = false ] || [ -e "$diagnostic_root" ] || \
            ! mv "$REMAGIC_TEST_ROOT" "$diagnostic_root"; then
            infrastructure_restored=false
            echo "acceptance isolation: could not preserve diagnostics" >&2
        else
            if sync; then
                echo "acceptance isolation: diagnostics retained at $diagnostic_root" >&2
            else
                mv "$diagnostic_root" "$REMAGIC_TEST_ROOT" >/dev/null 2>&1 || true
                infrastructure_restored=false
            fi
        fi
    else
        echo "acceptance isolation: diagnostics retained at $REMAGIC_TEST_ROOT" >&2
        finish_status=1
    fi
    if [ "$infrastructure_restored" = true ]; then
        remagic_test_release_lock || infrastructure_restored=false
    fi
    if [ "$infrastructure_restored" = false ]; then
        finish_status=1
        echo "acceptance isolation: recovery lock retained for the next run" >&2
    else
        REMAGIC_TEST_LOCKED=false
    fi
    REMAGIC_TEST_BEGUN=false
    REMAGIC_TEST_TRANSACTION_PREPARED=false
    REMAGIC_TEST_OVERRIDES_INSTALLED=false
    REMAGIC_TEST_SESSIONS_SAVED=false
    REMAGIC_TEST_AGENT_WAS_ACTIVE=false
    REMAGIC_TEST_PRODUCTION_FINGERPRINT=
    REMAGIC_TEST_FOREIGN_OVERRIDE=false
    return "$finish_status"
}
