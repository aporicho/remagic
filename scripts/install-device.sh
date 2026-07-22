#!/bin/sh
set -eu

[ "$(id -u)" -eq 0 ] || { echo "install-device.sh must run as root" >&2; exit 1; }

SOURCE_DIR=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
COMMON=$SOURCE_DIR/scripts/lib/deployment-common.sh
KOREADER_STORAGE_LIB=$SOURCE_DIR/scripts/lib/koreader-storage.sh
KOREADER_RELEASE_LIB=$SOURCE_DIR/scripts/lib/koreader-release.sh
REMAGIC_ACCEPTANCE_RECOVERY_LIB=$SOURCE_DIR/scripts/lib/device-test-recovery.sh
MAGICPAPER_FONT_CONTRACT=$SOURCE_DIR/scripts/lib/magicpaper-font-contract.sh
[ -r "$COMMON" ] || { echo "install-device.sh: deployment helper is missing" >&2; exit 1; }
[ -r "$KOREADER_STORAGE_LIB" ] || { echo "install-device.sh: KOReader storage helper is missing" >&2; exit 1; }
[ -r "$KOREADER_RELEASE_LIB" ] || { echo "install-device.sh: KOReader release helper is missing" >&2; exit 1; }
[ -r "$MAGICPAPER_FONT_CONTRACT" ] || {
    echo "install-device.sh: MagicPaper font contract is missing" >&2
    exit 1
}
. "$COMMON"
. "$KOREADER_STORAGE_LIB"
. "$KOREADER_RELEASE_LIB"
# shellcheck source=scripts/lib/magicpaper-font-contract.sh
. "$MAGICPAPER_FONT_CONTRACT"
umask 077

APP_ROOT=/home/root/apps/remagic
ADAPTER_ROOT=/home/root/apps/koreader-for-remagic
KOREADER_RELEASE_RECORD=$SOURCE_DIR/opt/koreader-for-remagic/deployment/current.env
KOREADER_LEGACY_ROOT=/home/root/apps/koreader
KOREADER_LEGACY_ROOTS=/home/root/.local/share/remagic-koreader/data:$KOREADER_LEGACY_ROOT:/home/root/.paperweight/services/koreader/koreader:/home/root/.config/koreader
KOREADER_DATA_ROOT=/home/root/.local/share/koreader-for-remagic/data
KOREADER_BACKUP_ROOT=/home/root/.local/state/koreader-for-remagic/backups
KOREADER_CONFIG_ROOT=/home/root/.config/koreader-for-remagic
KOREADER_CACHE_ROOT=/home/root/.cache/koreader-for-remagic
KOREADER_DEPLOYMENT_LOCK=/home/root/apps/.koreader-for-remagic.install.lock
KOREADER_MIGRATABLE_PATHS='settings.reader.lua settings.reader.lua.old history.lua defaults.custom.lua settings docsettings hashdocsettings history screenshots styletweaks sync clipboard data/dict data/tessdata'
STATE_ROOT=/home/root/.local/state/remagic/install
TRANSACTION_ROOT=$STATE_ROOT/transactions
ORIGINAL_ROOT=$STATE_ROOT/original
MANIFEST_ROOT=/home/root/.local/share/remagic/apps.d
UNIT_ROOT=/usr/lib/systemd/system
KOREADER_UNIT_DROPIN_REL=remagic-app@koreader.service.d/10-koreader-runtime.conf
KOREADER_UNIT_DROPIN=$UNIT_ROOT/$KOREADER_UNIT_DROPIN_REL
WANTS_ROOT=/etc/systemd/system/multi-user.target.wants
IN_PROGRESS=$STATE_ROOT/in-progress
MAGICPAPER_SCHEMA_VERSION=2
MAGICPAPER_SCHEMA_ROOT=/home/root/.local/state/magicpaper/.remagic-schema
MAGICPAPER_SCHEMA_FENCE=$MAGICPAPER_SCHEMA_ROOT/schema-ready
MAGICPAPER_SCHEMA_PENDING=$MAGICPAPER_SCHEMA_ROOT/pending.json

ROOT_WAS_READ_ONLY=false
ROOT_IS_WRITABLE=false
LOCK_HELD=false
INSTALL_COMMITTED=false
CURRENT_TXN=

detect_root_mount() {
    case "$(awk '$2 == "/" { print $4; exit }' /proc/mounts)" in
        ro|ro,*|*,ro|*,ro,*) ROOT_WAS_READ_ONLY=true ;;
    esac
}

ensure_root_writable() {
    [ "$ROOT_IS_WRITABLE" = true ] && return 0
    if [ "$ROOT_WAS_READ_ONLY" = true ]; then
        mount -o remount,rw /
    fi
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
        adapter:/home/root/apps/koreader-for-remagic:/home/root/apps/.koreader-for-remagic.stage.*:/home/root/apps/.koreader-for-remagic.rollback.*|\
        adapter:/home/root/apps/remagic-koreader:/home/root/apps/.remagic-koreader.stage.*:/home/root/apps/.remagic-koreader.rollback.*|\
        koreader:/home/root/apps/koreader:/home/root/apps/.koreader.stage.*:/home/root/apps/.koreader.rollback.*) ;;
        *) echo "install-device.sh: refusing unsafe switch journal: $record" >&2; return 1 ;;
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
        adapter:/home/root/apps/koreader-for-remagic:/home/root/apps/.koreader-for-remagic.uninstall.*|\
        adapter:/home/root/apps/remagic-koreader:/home/root/apps/.remagic-koreader.uninstall.*) ;;
        *) echo "install-device.sh: refusing unsafe removal journal: $record" >&2; return 1 ;;
    esac
    if [ -e "$backup" ] || [ -L "$backup" ]; then
        rm -rf "$live"
        mv "$backup" "$live"
    fi
}

restore_all_snapshots() {
    txn=$1
    for record in "$txn/snapshots/"*; do
        [ -d "$record" ] || continue
        restore_snapshot "$txn/snapshots" "$(basename "$record")" || return 1
    done
}

rollback_transaction() {
    txn=$1
    [ -d "$txn" ] || return 0
    case "$(transaction_status "$txn")" in committed|rolled-back) return 0 ;; esac
    echo "install-device.sh: rolling back transaction $(basename "$txn")" >&2
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
        *) echo "install-device.sh: unknown transaction kind: $kind" >&2; return 1 ;;
    esac
    restore_all_snapshots "$txn" || return 1
    remove_manager_runtime_files
    "$SYSTEMCTL" daemon-reload >/dev/null 2>&1 || return 1
    restore_stock_services || {
        echo "install-device.sh: stock service recovery needs attention" >&2
        return 1
    }
    if [ -e "$txn/was-riddle-active" ]; then
        start_if_loaded riddle-power-launcher.service || return 1
    fi
    if [ -e "$txn/was-remagicd-active" ]; then
        "$SYSTEMCTL" start remagicd.service >/dev/null 2>&1 || return 1
    fi
    if [ -e "$txn/was-agent-active" ]; then
        "$SYSTEMCTL" start magicpaper-agent.service >/dev/null 2>&1 || return 1
    fi
    printf '%s\n' rolled-back >"$txn/status"
    sync
    clear_deployment_guard_for_transaction "$IN_PROGRESS" "$txn"
}

finish_install() {
    status=$?
    trap - EXIT HUP INT TERM
    set +e
    if [ "$status" -ne 0 ] && [ -n "$CURRENT_TXN" ] && [ "$INSTALL_COMMITTED" = false ]; then
        ensure_root_writable || status=1
        rollback_transaction "$CURRENT_TXN" || status=1
    fi
    restore_root_mount || status=1
    if [ "$LOCK_HELD" = true ]; then
        release_directory_lock "$REMAGIC_INSTALL_LOCK"
    fi
    exit "$status"
}
trap finish_install EXIT
trap 'exit 1' HUP INT TERM

required_files='bin/remagicd
bin/remagicctl
bin/remagic-home
bin/remagic-runner
bin/remagic-vellum-worker
bin/remagic-display-host
lib/libquill.so
fonts/UIFont.ttf
share/build-info.txt
share/bundle.sha256
share/vellum-bootstrap.sh
shims/qtfb-shim.so
shims/LICENSE.qtfb-shim
opt/magicpaper/magicpaper
opt/magicpaper/fonts/851LakeusNightWriting.ttf
opt/magicpaper/fonts/ButterShiSan.ttf
opt/magicpaper/fonts/CoverageFallback.ttf
opt/magicpaper/fonts/FZPingXianYaSong.ttf
opt/koreader-for-remagic/deployment/current.env
opt/koreader-for-remagic/deployment/vendor.files
opt/koreader-for-remagic/deployment/vendor.sha256
scripts/lib/deployment-common.sh
scripts/lib/koreader-storage.sh
scripts/lib/koreader-release.sh
scripts/lib/magicpaper-font-contract.sh
scripts/lib/device-test-isolation.sh
scripts/lib/device-test-manifests.sh
scripts/lib/device-test-recovery.sh
scripts/remagic-recover
scripts/remagic-register
scripts/koreader-for-remagic
scripts/koreader-data-migrate
scripts/koreader-db-inspect
scripts/koreader-db-inspect.lua
scripts/koreader-library-sync
scripts/koreader-library-index.lua
scripts/koreader-not-running
scripts/remagic-lifecycle-protocol.lua
scripts/remagic-open-path.lua
scripts/magicpaper-remagic
scripts/magicpaper-agent-remagic
scripts/magicpaper-qtfb
scripts/magicpaper-data-migrate
scripts/remagic-schema-ready
scripts/magicpaper-env
scripts/uninstall-device.sh
scripts/device-acceptance-v2.sh
scripts/device-fault-acceptance-v2.sh
scripts/device-stress-acceptance-v2.sh
scripts/device-lock-acceptance-v2.sh
manifests/magicpaper.toml
manifests/koreader.toml
systemd/remagicd.service
systemd/remagic-display-host.service
systemd/remagic-home.service
systemd/remagic-app@.service
systemd/remagic-recover.service
systemd/magicpaper-agent.service
systemd/remagic-app@koreader.service.d/10-koreader-runtime.conf
testing/manifests/magicpaper.toml
testing/manifests/koreader.toml'

validate_bundle() {
    koreader_release_load "$KOREADER_RELEASE_RECORD" || {
        echo "install-device.sh: invalid KOReader release record" >&2
        return 1
    }
    KOREADER_PROGRAM_ROOT=$(koreader_release_vendor_root "$ADAPTER_ROOT")
    KOREADER_ADAPTER_RELEASE_ROOT=$(koreader_release_adapter_root "$ADAPTER_ROOT")
    KOREADER_SOURCE_PROGRAM_ROOT=$(koreader_release_vendor_root \
        "$SOURCE_DIR/opt/koreader-for-remagic")
    KOREADER_SOURCE_ADAPTER_ROOT=$(koreader_release_adapter_root \
        "$SOURCE_DIR/opt/koreader-for-remagic")
    export KOREADER_PROGRAM_ROOT KOREADER_ADAPTER_RELEASE_ROOT \
        KOREADER_SOURCE_PROGRAM_ROOT KOREADER_SOURCE_ADAPTER_ROOT
    grep -qx 'koreader=2026.03' "$SOURCE_DIR/share/build-info.txt" || {
        echo "install-device.sh: unsupported KOReader program version" >&2
        return 1
    }
    grep -Fqx "koreader-vendor-release=$KOREADER_VENDOR_RELEASE" \
        "$SOURCE_DIR/share/build-info.txt" && \
        grep -Fqx "koreader-adapter-release=$KOREADER_ADAPTER_RELEASE" \
            "$SOURCE_DIR/share/build-info.txt" || {
        echo "install-device.sh: KOReader release record and build metadata differ" >&2
        return 1
    }
    grep -qx 'v2026.03' "$KOREADER_SOURCE_PROGRAM_ROOT/git-rev" || {
        echo "install-device.sh: KOReader git-rev does not match v2026.03" >&2
        return 1
    }
    grep -qx "magicpaper-ui-font-sha256=$MAGICPAPER_UI_FONT_SHA256" \
        "$SOURCE_DIR/share/build-info.txt" || {
        echo "install-device.sh: MagicPaper UI font build contract is invalid" >&2
        return 1
    }
    for manifest in "$SOURCE_DIR/manifests/koreader.toml" \
        "$SOURCE_DIR/testing/manifests/koreader.toml"; do
        grep -Fqx "working_dir = \"$KOREADER_PROGRAM_ROOT\"" "$manifest" && \
            grep -Fqx "KOREADER_DIR = \"$KOREADER_PROGRAM_ROOT\"" "$manifest" && \
            grep -Fqx "exec = \"$KOREADER_ADAPTER_RELEASE_ROOT/bin/koreader-for-remagic\"" "$manifest" && \
            grep -Fqx "directories = [\"$KOREADER_ADAPTER_RELEASE_ROOT/share/fonts\"]" "$manifest" && \
            grep -Fqx 'background_execution = "freeze"' "$manifest" || {
            echo "install-device.sh: KOReader manifest does not use the pinned releases: $manifest" >&2
            return 1
        }
    done
    grep -qx 'KO_HOME = "/home/root/.local/share/koreader-for-remagic/data"' \
        "$SOURCE_DIR/manifests/koreader.toml" || {
        echo "install-device.sh: production KOReader data root contract is invalid" >&2
        return 1
    }
    [ ! -e "$KOREADER_SOURCE_PROGRAM_ROOT/update_once.marker" ] && \
        [ ! -L "$KOREADER_SOURCE_PROGRAM_ROOT/update_once.marker" ] || {
        echo "install-device.sh: KOReader update_once.marker was not consumed" >&2
        return 1
    }
    [ -d "$KOREADER_SOURCE_PROGRAM_ROOT/plugins/terminal.koplugin" ] && \
        [ ! -L "$KOREADER_SOURCE_PROGRAM_ROOT/plugins/terminal.koplugin" ] || {
        echo "install-device.sh: official KOReader terminal plugin is missing" >&2
        return 1
    }
    [ ! -e "$KOREADER_SOURCE_PROGRAM_ROOT/fonts/remagic" ] && \
        { [ ! -d "$KOREADER_SOURCE_PROGRAM_ROOT/patches" ] || \
          [ -z "$(find "$KOREADER_SOURCE_PROGRAM_ROOT/patches" -maxdepth 1 \
              -name '*remagic*.lua' -print -quit)" ]; } || {
        echo "install-device.sh: ReMagic assets leaked into the KOReader vendor tree" >&2
        return 1
    }
    koreader_release_verify "$SOURCE_DIR/opt/koreader-for-remagic" || {
        echo "install-device.sh: KOReader release integrity verification failed" >&2
        return 1
    }
    for relative in \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/bin/koreader-for-remagic" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/libexec/koreader-data-migrate" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/libexec/koreader-db-inspect" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/libexec/koreader-db-inspect.lua" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/libexec/koreader-library-sync" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/libexec/koreader-library-index.lua" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/libexec/koreader-not-running" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/libexec/remagic-lifecycle-protocol.lua" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/libexec/remagic-open-path.lua" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/share/patches/10-remagic-environment.lua" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/share/patches/20-remagic-policy.lua" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/share/patches/21-remagic-lifecycle-v2.lua" \
        "opt/koreader-for-remagic/adapter/releases/$KOREADER_ADAPTER_RELEASE/share/fonts/fonts.sha256"; do
        required_files="$required_files
$relative"
    done
    printf '%s\n' "$required_files" | while IFS= read -r relative; do
        [ -n "$relative" ] || continue
        path=$SOURCE_DIR/$relative
        [ -f "$path" ] && [ ! -L "$path" ] && [ -s "$path" ] || {
            echo "install-device.sh: missing, empty, or unsafe bundle file: $relative" >&2
            exit 1
        }
        [ "$(stat -c %u "$path" 2>/dev/null || echo unknown)" = 0 ] || {
            echo "install-device.sh: bundle file is not root-owned: $relative" >&2
            exit 1
        }
    done
    magicpaper_verify_font_sha256 "$MAGICPAPER_UI_FONT_SHA256" \
        "$SOURCE_DIR/opt/magicpaper/fonts/FZPingXianYaSong.ttf" \
        "install-device.sh: bundled MagicPaper Fangzheng UI font" || return 1
    [ -z "$(find "$SOURCE_DIR" -type l -print -quit)" ] || {
        echo "install-device.sh: bundle must not contain symbolic links" >&2
        return 1
    }
    [ -z "$(find "$SOURCE_DIR" ! -type f ! -type d -print -quit)" ] || {
        echo "install-device.sh: bundle contains a special filesystem object" >&2
        return 1
    }
    find "$SOURCE_DIR" \( -type f -o -type d \) -print | while IFS= read -r path; do
        [ "$(stat -c %u "$path" 2>/dev/null || echo unknown)" = 0 ] || {
            echo "install-device.sh: bundle path is not root-owned: $path" >&2
            exit 1
        }
    done
    (cd "$SOURCE_DIR" && sha256sum -c share/bundle.sha256 >/dev/null) || {
        echo "install-device.sh: bundle checksum verification failed" >&2
        return 1
    }
    for executable in \
        bin/remagicd bin/remagicctl bin/remagic-home bin/remagic-runner \
        bin/remagic-vellum-worker bin/remagic-display-host \
        opt/magicpaper/magicpaper \
        scripts/remagic-recover scripts/koreader-for-remagic \
        scripts/remagic-register \
        scripts/koreader-data-migrate scripts/koreader-db-inspect \
        scripts/koreader-library-sync scripts/koreader-not-running \
        scripts/magicpaper-remagic scripts/magicpaper-agent-remagic \
        scripts/magicpaper-qtfb scripts/magicpaper-data-migrate scripts/remagic-schema-ready \
        scripts/install-device.sh scripts/uninstall-device.sh; do
        [ -x "$SOURCE_DIR/$executable" ] || {
            echo "install-device.sh: executable bit missing: $executable" >&2
            return 1
        }
    done
    for executable in \
        "$KOREADER_SOURCE_ADAPTER_ROOT/bin/koreader-for-remagic" \
        "$KOREADER_SOURCE_ADAPTER_ROOT/libexec/koreader-data-migrate" \
        "$KOREADER_SOURCE_ADAPTER_ROOT/libexec/koreader-db-inspect" \
        "$KOREADER_SOURCE_ADAPTER_ROOT/libexec/koreader-library-sync" \
        "$KOREADER_SOURCE_ADAPTER_ROOT/libexec/koreader-not-running"; do
        [ -x "$executable" ] || {
            echo "install-device.sh: adapter executable bit missing: $executable" >&2
            return 1
        }
    done
    for artifact in \
        bin/remagicd bin/remagicctl bin/remagic-home bin/remagic-runner \
        bin/remagic-vellum-worker bin/remagic-display-host lib/libquill.so \
        shims/qtfb-shim.so opt/magicpaper/magicpaper; do
        assert_aarch64_elf "$SOURCE_DIR/$artifact" || return 1
    done
    assert_aarch64_elf "$KOREADER_SOURCE_PROGRAM_ROOT/luajit" || return 1
    expected_quill=$(sed -n 's/^libquill-sha256=//p' "$SOURCE_DIR/share/build-info.txt")
    expected_host=$(sed -n 's/^display-host-sha256=//p' "$SOURCE_DIR/share/build-info.txt")
    actual_quill=$(sha256sum "$SOURCE_DIR/lib/libquill.so" | awk '{print $1}')
    actual_host=$(sha256sum "$SOURCE_DIR/bin/remagic-display-host" | awk '{print $1}')
    [ -n "$expected_quill" ] && [ "$expected_quill" = "$actual_quill" ] && \
        [ -n "$expected_host" ] && [ "$expected_host" = "$actual_host" ] || {
        echo "install-device.sh: display-host/libquill build contract mismatch" >&2
        return 1
    }
}

preflight_space() {
    source_kb=$(du -sk "$SOURCE_DIR" | awk '{print $1}')
    data_kb=$(du -sk "$KOREADER_DATA_ROOT" 2>/dev/null | awk '{print $1}')
    data_kb=${data_kb:-0}
    backup_kb=$(du -sk "$KOREADER_BACKUP_ROOT" 2>/dev/null | awk '{print $1}')
    backup_kb=${backup_kb:-0}
    legacy_kb=$(deployment_koreader_legacy_kb \
        "$KOREADER_LEGACY_ROOTS" "$KOREADER_MIGRATABLE_PATHS")
    required_kb=$(deployment_home_space_required \
        "$source_kb" "$data_kb" "$backup_kb" "$legacy_kb")
    available_kb=$(df -Pk /home | awk 'NR == 2 { print $4 }')
    deployment_home_space_sufficient "$required_kb" "${available_kb:-unknown}" || {
        echo "install-device.sh: need ${required_kb} KiB free on /home, have ${available_kb:-unknown}" >&2
        return 1
    }
    root_available=$(df -Pk / | awk 'NR == 2 { print $4 }')
    [ -n "$root_available" ] && [ "$root_available" -ge 8192 ] || {
        echo "install-device.sh: at least 8 MiB free is required on /" >&2
        return 1
    }
}

assert_safe_koreader_storage() {
    for storage_root in "$KOREADER_DATA_ROOT" "$KOREADER_BACKUP_ROOT"; do
        deployment_assert_safe_storage_tree "$storage_root" || {
            echo "install-device.sh: unsafe KOReader storage path or object: $storage_root" >&2
            return 1
        }
    done

    old_ifs=$IFS
    IFS=:
    for legacy_root in $KOREADER_LEGACY_ROOTS; do
        IFS=$old_ifs
        if deployment_assert_safe_directory_path "$legacy_root" 2>/dev/null && \
           [ -d "$legacy_root" ] && [ ! -L "$legacy_root" ]; then
            for name in $KOREADER_MIGRATABLE_PATHS; do
                path=$legacy_root/$name
                [ -e "$path" ] || [ -L "$path" ] || continue
                [ -L "$path" ] && continue
                deployment_assert_safe_directory_path "${path%/*}" 2>/dev/null || continue
                unsafe=$(find "$path" ! -type f ! -type d ! -type l -print -quit \
                    2>/dev/null || printf unreadable)
                [ -z "$unsafe" ] || {
                    echo "install-device.sh: unsafe legacy KOReader data object: $unsafe" >&2
                    return 1
                }
            done
        fi
        IFS=:
    done
    IFS=$old_ifs
}

stage_file() {
    mode=$1
    source=$2
    target=$3
    mkdir -p "$(dirname "$target")"
    cp -f "$source" "$target"
    chmod "$mode" "$target"
    chown 0:0 "$target"
}

stage_manager() {
    stage=$1
    rm -rf "$stage"
    mkdir -p "$stage"
    for binary in remagicd remagic-home remagic-runner remagicctl remagic-vellum-worker remagic-display-host; do
        stage_file 0755 "$SOURCE_DIR/bin/$binary" "$stage/bin/$binary"
    done
    stage_file 0755 "$SOURCE_DIR/lib/libquill.so" "$stage/lib/libquill.so"
    stage_file 0755 "$SOURCE_DIR/shims/qtfb-shim.so" "$stage/shims/qtfb-shim.so"
    stage_file 0644 "$SOURCE_DIR/shims/LICENSE.qtfb-shim" "$stage/share/LICENSE.qtfb-shim"
    for helper in deployment-common.sh koreader-release.sh remagic-recover remagic-register magicpaper-remagic \
        magicpaper-agent-remagic magicpaper-qtfb magicpaper-data-migrate remagic-schema-ready \
        uninstall-device.sh magicpaper-env; do
        source=$SOURCE_DIR/scripts/$helper
        case "$helper" in
            deployment-common.sh|koreader-release.sh) source=$SOURCE_DIR/scripts/lib/$helper ;;
        esac
        mode=0755
        [ "$helper" = magicpaper-env ] && mode=0644
        stage_file "$mode" "$source" "$stage/libexec/$helper"
    done
    stage_file 0755 "$SOURCE_DIR/scripts/lib/device-test-isolation.sh" \
        "$stage/libexec/device-test-isolation.sh"
    stage_file 0755 "$SOURCE_DIR/scripts/lib/device-test-manifests.sh" "$stage/libexec/device-test-manifests.sh"
    stage_file 0755 "$SOURCE_DIR/scripts/lib/device-test-recovery.sh" \
        "$stage/libexec/device-test-recovery.sh"
    for test_script in device-acceptance-v2.sh device-fault-acceptance-v2.sh device-stress-acceptance-v2.sh device-lock-acceptance-v2.sh; do
        stage_file 0755 "$SOURCE_DIR/scripts/$test_script" "$stage/share/$test_script"
    done
    stage_file 0644 "$SOURCE_DIR/share/build-info.txt" "$stage/share/build-info.txt"
    stage_file 0644 "$SOURCE_DIR/share/bundle.sha256" "$stage/share/bundle.sha256"
    stage_file 0644 "$SOURCE_DIR/share/vellum-bootstrap.sh" "$stage/share/vellum-bootstrap.sh"
    for unit in remagicd.service remagic-display-host.service remagic-home.service \
        remagic-app@.service remagic-recover.service magicpaper-agent.service; do
        stage_file 0644 "$SOURCE_DIR/systemd/$unit" "$stage/share/systemd/$unit"
    done
    stage_file 0644 "$SOURCE_DIR/systemd/$KOREADER_UNIT_DROPIN_REL" \
        "$stage/share/systemd/$KOREADER_UNIT_DROPIN_REL"
    for manifest in magicpaper.toml koreader.toml; do
        stage_file 0644 "$SOURCE_DIR/testing/manifests/$manifest" \
            "$stage/share/testing/manifests/$manifest"
    done
    stage_file 0644 "$SOURCE_DIR/fonts/UIFont.ttf" "$stage/fonts/UIFont.ttf"
    stage_file 0755 "$SOURCE_DIR/opt/magicpaper/magicpaper" "$stage/opt/magicpaper/magicpaper"
    for font_name in 851LakeusNightWriting.ttf ButterShiSan.ttf \
        CoverageFallback.ttf FZPingXianYaSong.ttf; do
        stage_file 0644 "$SOURCE_DIR/opt/magicpaper/fonts/$font_name" \
            "$stage/opt/magicpaper/fonts/$font_name"
    done
    chown -R 0:0 "$stage"
}

stage_adapter() {
    stage=$1
    rm -rf "$stage"
    cp -a "$SOURCE_DIR/opt/koreader-for-remagic" "$stage"

    # Preserve exactly one post-commit rollback candidate.  Release files are
    # immutable, so hard links retain the old content without doubling space.
    previous_record=$ADAPTER_ROOT/deployment/current.env
    if [ -d "$ADAPTER_ROOT" ] && [ ! -L "$ADAPTER_ROOT" ] && \
       koreader_release_load "$previous_record"; then
        previous_vendor=$KOREADER_VENDOR_RELEASE
        previous_adapter=$KOREADER_ADAPTER_RELEASE
        koreader_release_load "$KOREADER_RELEASE_RECORD" || return 1
        if [ "$previous_vendor:$previous_adapter" != \
             "$KOREADER_VENDOR_RELEASE:$KOREADER_ADAPTER_RELEASE" ]; then
            previous_vendor_root=$ADAPTER_ROOT/vendor/releases/$previous_vendor
            previous_adapter_root=$ADAPTER_ROOT/adapter/releases/$previous_adapter
            [ -d "$previous_vendor_root" ] && [ ! -L "$previous_vendor_root" ] && \
                [ -d "$previous_adapter_root" ] && [ ! -L "$previous_adapter_root" ] || {
                echo "install-device.sh: previous KOReader release is incomplete" >&2
                return 1
            }
            if [ ! -e "$stage/vendor/releases/$previous_vendor" ]; then
                cp -al "$previous_vendor_root" \
                    "$stage/vendor/releases/$previous_vendor"
            fi
            if [ ! -e "$stage/adapter/releases/$previous_adapter" ]; then
                cp -al "$previous_adapter_root" \
                    "$stage/adapter/releases/$previous_adapter"
            fi
            printf 'vendor_release=%s\nadapter_release=%s\n' \
                "$previous_vendor" "$previous_adapter" \
                >"$stage/deployment/previous.env"
        fi
    fi
    koreader_release_load "$KOREADER_RELEASE_RECORD" || return 1
    chown -R 0:0 "$stage"
    koreader_release_verify "$stage"
}

capture_original_state() {
    captured=$ORIGINAL_ROOT/captured
    captured_new=$ORIGINAL_ROOT/.captured.$$.new
    original_want_usr=/usr/lib/systemd/system/multi-user.target.wants/riddle-power-launcher.service
    original_want_etc=$WANTS_ROOT/riddle-power-launcher.service
    if [ -e "$captured" ] || [ -L "$captured" ]; then
        [ -f "$captured" ] && [ ! -L "$captured" ] || return 1
        snapshot_record_is_complete "$ORIGINAL_ROOT/snapshots" riddle_want_usr \
            "$original_want_usr" || return 1
        snapshot_record_is_complete "$ORIGINAL_ROOT/snapshots" riddle_want_etc \
            "$original_want_etc" || return 1
        [ -f "$ORIGINAL_ROOT/launcher-enabled" ] && \
            [ ! -L "$ORIGINAL_ROOT/launcher-enabled" ] || return 1
        if [ -e "$ORIGINAL_ROOT/launcher-active" ] || [ -L "$ORIGINAL_ROOT/launcher-active" ]; then
            [ -f "$ORIGINAL_ROOT/launcher-active" ] && \
                [ ! -L "$ORIGINAL_ROOT/launcher-active" ] || return 1
        fi
        return 0
    fi
    mkdir -p "$ORIGINAL_ROOT/snapshots"
    snapshot_path "$ORIGINAL_ROOT/snapshots" riddle_want_usr \
        "$original_want_usr"
    snapshot_path "$ORIGINAL_ROOT/snapshots" riddle_want_etc \
        "$original_want_etc"
    launcher_enabled=$("$SYSTEMCTL" is-enabled riddle-power-launcher.service 2>/dev/null || true)
    if [ -r "$STATE_ROOT/previous.env" ] && grep -q '^MAGICPAPER_POWER_LAUNCHER=enabled$' "$STATE_ROOT/previous.env"; then
        launcher_enabled=enabled
    fi
    printf '%s\n' "$launcher_enabled" >"$ORIGINAL_ROOT/launcher-enabled"
    unit_is_active riddle-power-launcher.service && : >"$ORIGINAL_ROOT/launcher-active"
    chmod -R go-rwx "$ORIGINAL_ROOT"
    # `captured` is the authority used by every later upgrade and uninstall.
    # Publish it only after both required records and their metadata are durable.
    sync
    rm -f "$captured_new"
    : >"$captured_new"
    sync
    mv "$captured_new" "$captured"
    sync
}

snapshot_transaction_paths() {
    snapshots=$1/snapshots
    mkdir -p "$snapshots"
    snapshot_path "$snapshots" unit_remagicd "$UNIT_ROOT/remagicd.service"
    snapshot_path "$snapshots" unit_display "$UNIT_ROOT/remagic-display-host.service"
    snapshot_path "$snapshots" unit_home "$UNIT_ROOT/remagic-home.service"
    snapshot_path "$snapshots" unit_app "$UNIT_ROOT/remagic-app@.service"
    snapshot_path "$snapshots" unit_recover "$UNIT_ROOT/remagic-recover.service"
    snapshot_path "$snapshots" unit_agent "$UNIT_ROOT/magicpaper-agent.service"
    snapshot_path "$snapshots" unit_koreader_dropin "$KOREADER_UNIT_DROPIN"
    snapshot_path "$snapshots" unit_runtime "$UNIT_ROOT/remagic-runtime.service"
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
}

snapshot_koreader_storage() {
    txn=$1
    snapshot_path "$txn/snapshots" ko_data_root "$KOREADER_DATA_ROOT"
    snapshot_path "$txn/snapshots" ko_backup_root "$KOREADER_BACKUP_ROOT"
}

publish_file() {
    mode=$1
    source=$2
    target=$3
    temp=$(dirname "$target")/.remagic-$(basename "$CURRENT_TXN").$(basename "$target").new
    mkdir -p "$(dirname "$target")"
    rm -f "$temp"
    cp "$source" "$temp"
    chmod "$mode" "$temp"
    chown 0:0 "$temp"
    mv -f "$temp" "$target"
}

publish_symlink() {
    target=$1
    path=$2
    temp=$(dirname "$path")/.remagic-$(basename "$CURRENT_TXN").$(basename "$path").new
    mkdir -p "$(dirname "$path")"
    rm -f "$temp"
    ln -s "$target" "$temp"
    mv -f "$temp" "$path"
}

cleanup_transaction_backups() {
    txn=$1
    for name in app adapter koreader; do
        record=$txn/switch-$name
        [ -r "$record/backup" ] || continue
        backup=$(sed -n '1p' "$record/backup")
        stage=$(sed -n '1p' "$record/stage")
        canonical_absolute_path "$stage" && canonical_absolute_path "$backup" || return 1
        case "$name:$stage:$backup" in
            app:/home/root/apps/.remagic.stage.*:/home/root/apps/.remagic.rollback.*|\
            adapter:/home/root/apps/.koreader-for-remagic.stage.*:/home/root/apps/.koreader-for-remagic.rollback.*|\
            adapter:/home/root/apps/.remagic-koreader.stage.*:/home/root/apps/.remagic-koreader.rollback.*|\
            koreader:/home/root/apps/.koreader.stage.*:/home/root/apps/.koreader.rollback.*) ;;
            *) echo "install-device.sh: refusing unsafe cleanup journal: $record" >&2; return 1 ;;
        esac
        rm -rf "$backup" "$stage"
    done
    rm -rf "$txn/snapshots"
}

assert_optional_symlink() {
    path=$1
    if [ -e "$path" ] && [ ! -L "$path" ]; then
        echo "install-device.sh: refusing to replace non-symlink wants entry: $path" >&2
        return 1
    fi
}

validate_bundle
assert_safe_koreader_storage
preflight_space
acquire_directory_lock "$REMAGIC_INSTALL_LOCK" install-device.sh
LOCK_HELD=true
wait_for_lock_barrier "$REMAGIC_RECOVERY_LOCK" install-device.sh
cleanup_stale_acceptance_environment
detect_root_mount
ensure_root_writable
mkdir -p "$TRANSACTION_ROOT"
chmod 0700 "$STATE_ROOT" "$TRANSACTION_ROOT"

# Complete rollback from a power loss before starting another transaction.
# Newest-first ordering is required if an older release ever admitted a
# nested install/uninstall journal.
transaction_names=$(list_deployment_transactions_newest_first "$TRANSACTION_ROOT")
for transaction_name in $transaction_names; do
    abandoned=$TRANSACTION_ROOT/$transaction_name
    case "$(transaction_status "$abandoned")" in
        committed|rolled-back)
            cleanup_finished_deployment_transaction "$abandoned" || {
                echo "install-device.sh: could not retire completed transaction: $abandoned" >&2
                exit 1
            }
            clear_deployment_guard_for_transaction "$IN_PROGRESS" "$abandoned"
            continue
            ;;
    esac
    rollback_transaction "$abandoned"
done

TXN_ID=$(date +%Y%m%d-%H%M%S 2>/dev/null || echo unknown)-$$
CURRENT_TXN=$TRANSACTION_ROOT/$TXN_ID
mkdir -p "$CURRENT_TXN"
printf '%s\n' preparing >"$CURRENT_TXN/status"
printf '%s\n' install >"$CURRENT_TXN/kind"
printf '%s\n' "$TXN_ID" >"$IN_PROGRESS"
unit_is_active remagicd.service && : >"$CURRENT_TXN/was-remagicd-active"
unit_is_active magicpaper-agent.service && : >"$CURRENT_TXN/was-agent-active"
unit_is_active riddle-power-launcher.service && : >"$CURRENT_TXN/was-riddle-active"
capture_original_state
snapshot_transaction_paths "$CURRENT_TXN"
assert_optional_symlink "$WANTS_ROOT/remagicd.service"
assert_optional_symlink "$WANTS_ROOT/magicpaper-agent.service"
assert_optional_symlink "$UNIT_ROOT/multi-user.target.wants/remagicd.service"
assert_optional_symlink "$UNIT_ROOT/multi-user.target.wants/magicpaper-agent.service"
assert_optional_symlink "$WANTS_ROOT/remagic-runtime.service"
assert_optional_symlink "$UNIT_ROOT/multi-user.target.wants/remagic-runtime.service"
assert_optional_symlink "$UNIT_ROOT/multi-user.target.wants/riddle-power-launcher.service"
assert_optional_symlink "$WANTS_ROOT/riddle-power-launcher.service"

APP_STAGE=/home/root/apps/.remagic.stage.$TXN_ID
APP_BACKUP=/home/root/apps/.remagic.rollback.$TXN_ID
ADAPTER_STAGE=/home/root/apps/.koreader-for-remagic.stage.$TXN_ID
ADAPTER_BACKUP=/home/root/apps/.koreader-for-remagic.rollback.$TXN_ID
stage_manager "$APP_STAGE"
stage_adapter "$ADAPTER_STAGE"

# Everything needed to undo the first live mutation must be durable before a
# boot-activation link is removed.  In particular, never rely on the sync after
# the removal to order snapshot publication ahead of that removal.
sync

# Disable boot activation before changing any executable.  The links are
# restored from the transaction snapshot if anything below fails.
rm -f "$WANTS_ROOT/remagicd.service" "$WANTS_ROOT/magicpaper-agent.service" \
    "$UNIT_ROOT/multi-user.target.wants/remagicd.service" \
    "$UNIT_ROOT/multi-user.target.wants/magicpaper-agent.service" \
    "$WANTS_ROOT/remagic-runtime.service" \
    "$UNIT_ROOT/multi-user.target.wants/remagic-runtime.service" \
    "$UNIT_ROOT/multi-user.target.wants/riddle-power-launcher.service" \
    "$WANTS_ROOT/riddle-power-launcher.service"
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
: >"$CURRENT_TXN/services-stopped"

# KOReader and its agent are now stopped, so these whole-tree snapshots are a
# consistent rollback boundary for writable data and migration backups.
assert_safe_koreader_storage
preflight_space
snapshot_koreader_storage "$CURRENT_TXN"
transactional_tree_switch "$CURRENT_TXN/switch-app" "$APP_ROOT" "$APP_STAGE" "$APP_BACKUP"
transactional_tree_switch "$CURRENT_TXN/switch-adapter" "$ADAPTER_ROOT" "$ADAPTER_STAGE" "$ADAPTER_BACKUP"

koreader_release_verify "$ADAPTER_ROOT" || {
    echo "install-device.sh: live KOReader release verification failed" >&2
    exit 1
}

KOREADER_DIR=$KOREADER_PROGRAM_ROOT \
KOREADER_ADAPTER_EXEC=$KOREADER_ADAPTER_RELEASE_ROOT/bin/koreader-for-remagic \
    "$KOREADER_ADAPTER_RELEASE_ROOT/libexec/koreader-not-running"
legacy_sources=$KOREADER_LEGACY_ROOTS
KOREADER_DIR=$KOREADER_PROGRAM_ROOT \
KOREADER_DATA_DIR=$KOREADER_DATA_ROOT \
KO_HOME=$KOREADER_DATA_ROOT \
KOREADER_LIBEXEC_DIR=$KOREADER_ADAPTER_RELEASE_ROOT/libexec \
KOREADER_BACKUP_ROOT=$KOREADER_BACKUP_ROOT \
KOREADER_LEGACY_DATA_DIRS=$legacy_sources \
    "$KOREADER_ADAPTER_RELEASE_ROOT/libexec/koreader-data-migrate"
assert_safe_koreader_storage
if [ ! -e "$KOREADER_DATA_ROOT/settings.reader.lua" ] && \
   [ ! -L "$KOREADER_DATA_ROOT/settings.reader.lua" ] && \
   [ ! -e "$KOREADER_DATA_ROOT/settings.reader.lua.old" ] && \
   [ ! -L "$KOREADER_DATA_ROOT/settings.reader.lua.old" ]; then
    deployment_assert_safe_directory_path "$KOREADER_DATA_ROOT"
    mkdir -p "$KOREADER_DATA_ROOT"
    deployment_assert_safe_storage_tree "$KOREADER_DATA_ROOT"
    settings_new=$KOREADER_DATA_ROOT/.settings.reader.lua.remagic-new.$$
    [ ! -e "$settings_new" ] && [ ! -L "$settings_new" ] || {
        echo "install-device.sh: temporary KOReader settings path already exists" >&2
        exit 1
    }
    printf '%s\n' '-- ReMagic KOReader settings' 'return {' \
        '    ["language"] = "zh_CN",' '}' >"$settings_new"
    chmod 0600 "$settings_new"
    mv "$settings_new" "$KOREADER_DATA_ROOT/settings.reader.lua"
fi

# systemd establishes KOReader's read-only home namespace before runner can
# create its XDG directories. Publish the narrow writable mount points in the
# host namespace first; existing user data and modes are left untouched.
koreader_runtime_prepare_writable_paths \
    "$KOREADER_CONFIG_ROOT" "$KOREADER_CACHE_ROOT" "$KOREADER_DEPLOYMENT_LOCK" || {
    echo "install-device.sh: could not prepare KOReader writable mount points" >&2
    exit 1
}

publish_file 0644 "$SOURCE_DIR/manifests/magicpaper.toml" "$MANIFEST_ROOT/magicpaper.toml"
publish_file 0644 "$SOURCE_DIR/manifests/koreader.toml" "$MANIFEST_ROOT/koreader.toml"
publish_file 0644 "$SOURCE_DIR/systemd/remagicd.service" "$UNIT_ROOT/remagicd.service"
publish_file 0644 "$SOURCE_DIR/systemd/remagic-display-host.service" "$UNIT_ROOT/remagic-display-host.service"
publish_file 0644 "$SOURCE_DIR/systemd/remagic-home.service" "$UNIT_ROOT/remagic-home.service"
publish_file 0644 "$SOURCE_DIR/systemd/remagic-app@.service" "$UNIT_ROOT/remagic-app@.service"
publish_file 0644 "$SOURCE_DIR/systemd/remagic-recover.service" "$UNIT_ROOT/remagic-recover.service"
publish_file 0644 "$SOURCE_DIR/systemd/magicpaper-agent.service" "$UNIT_ROOT/magicpaper-agent.service"
publish_file 0644 "$SOURCE_DIR/systemd/$KOREADER_UNIT_DROPIN_REL" "$KOREADER_UNIT_DROPIN"
rm -f "$UNIT_ROOT/remagic-runtime.service"
cmp -s "$SOURCE_DIR/systemd/$KOREADER_UNIT_DROPIN_REL" "$KOREADER_UNIT_DROPIN" || {
    echo "install-device.sh: KOReader read-only runtime policy did not publish" >&2
    exit 1
}

"$SYSTEMCTL" daemon-reload
restore_stock_services
"$SYSTEMCTL" restart remagicd.service
wait_unit_active remagicd.service 100
"$APP_ROOT/bin/remagicctl" status >/dev/null
if [ ! -e "$MAGICPAPER_SCHEMA_PENDING" ] && \
   [ ! -L "$MAGICPAPER_SCHEMA_PENDING" ] && \
   "$APP_ROOT/libexec/remagic-schema-ready" \
       "$MAGICPAPER_SCHEMA_FENCE" magicpaper "$MAGICPAPER_SCHEMA_VERSION"; then
    "$SYSTEMCTL" start magicpaper-agent.service
    wait_unit_active magicpaper-agent.service 100
else
    echo "MagicPaper agent deferred until its supervised schema is ready" >&2
fi
[ ! -e "$KOREADER_PROGRAM_ROOT/update_once.marker" ] && \
    [ ! -L "$KOREADER_PROGRAM_ROOT/update_once.marker" ] || {
    echo "install-device.sh: KOReader update_once.marker survived commit preparation" >&2
    exit 1
}
[ -d "$KOREADER_PROGRAM_ROOT/plugins/terminal.koplugin" ] && \
    [ ! -L "$KOREADER_PROGRAM_ROOT/plugins/terminal.koplugin" ] || {
    echo "install-device.sh: official KOReader terminal plugin is missing" >&2
    exit 1
}
grep -qx 'v2026.03' "$KOREADER_PROGRAM_ROOT/git-rev" || {
    echo "install-device.sh: live KOReader git-rev does not match v2026.03" >&2
    exit 1
}
cmp -s "$KOREADER_SOURCE_PROGRAM_ROOT/reader.lua" "$KOREADER_PROGRAM_ROOT/reader.lua" && \
    cmp -s "$KOREADER_SOURCE_PROGRAM_ROOT/luajit" "$KOREADER_PROGRAM_ROOT/luajit" && \
    koreader_release_verify "$ADAPTER_ROOT" || {
    echo "install-device.sh: KOReader v2026.03 program-tree verification failed" >&2
    exit 1
}

# /etc is a volatile overlay on reMarkable OS.  Publish boot wants alongside
# the immutable unit files so a normal reboot cannot silently discard them.
publish_symlink /usr/lib/systemd/system/remagicd.service "$UNIT_ROOT/multi-user.target.wants/remagicd.service"
publish_symlink /usr/lib/systemd/system/magicpaper-agent.service "$UNIT_ROOT/multi-user.target.wants/magicpaper-agent.service"
sync
printf '%s\n' committed >"$CURRENT_TXN/status"
sync
clear_deployment_guard_for_transaction "$IN_PROGRESS" "$CURRENT_TXN"
INSTALL_COMMITTED=true
cleanup_transaction_backups "$CURRENT_TXN" || true
restore_root_mount
release_directory_lock "$REMAGIC_INSTALL_LOCK"
LOCK_HELD=false

echo "ReMagic installed transactionally. The original interface remains the boot default."
echo "Triple-press power to open the manager."
