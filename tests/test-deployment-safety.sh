#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d /tmp/remagic-deployment-test.XXXXXX)
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT HUP INT TERM

STATE=$TMP/state
mkdir -p "$STATE/active" "$STATE/loaded" "$STATE/enabled"
FAKE=$TMP/systemctl
cat >"$FAKE" <<'EOF'
#!/bin/sh
set -eu
state=${FAKE_SYSTEMCTL_STATE:?}
command=$1
shift
last=
for argument in "$@"; do last=$argument; done
case $command in
    is-active)
        if [ -e "$state/active/$last" ]; then echo active; exit 0; fi
        echo inactive
        exit 3
        ;;
    is-enabled)
        if [ -e "$state/enabled/$last" ]; then echo enabled; exit 0; fi
        echo disabled
        exit 1
        ;;
    show)
        if [ -e "$state/loaded/$last" ] || [ -e "$state/active/$last" ]; then echo loaded; else echo not-found; fi
        ;;
    list-units)
        for unit in "$state"/active/*.service; do
            [ -e "$unit" ] || continue
            printf '%s loaded active running fixture\n' "${unit##*/}"
        done
        ;;
    stop)
        for unit in "$@"; do rm -f "$state/active/$unit"; done
        ;;
    kill|reset-failed|unmask|mask|daemon-reload) ;;
    start|restart)
        touch "$state/active/$last" "$state/loaded/$last"
        ;;
    *) echo "unexpected fake systemctl command: $command" >&2; exit 2 ;;
esac
EOF
chmod 0755 "$FAKE"
export FAKE_SYSTEMCTL_STATE=$STATE
SYSTEMCTL=$FAKE
REMAGIC_INSTALL_LOCK=$TMP/install.lock
REMAGIC_ACCEPTANCE_LOCK=$TMP/acceptance.lock
REMAGIC_SYSTEMD_RUNTIME_ROOT=$TMP/runtime-systemd
export SYSTEMCTL REMAGIC_INSTALL_LOCK REMAGIC_ACCEPTANCE_LOCK REMAGIC_SYSTEMD_RUNTIME_ROOT
. "$ROOT/scripts/lib/deployment-common.sh"
. "$ROOT/scripts/lib/koreader-storage.sh"
. "$ROOT/scripts/lib/magicpaper-font-contract.sh"

[ "$MAGICPAPER_UI_FONT_SHA256" = \
    dbbdf59d7035d980abecf4f820e615b72107865a00f6eb41a1bbb9d9d1492fd1 ] || {
    echo "MagicPaper UI font content identity changed unexpectedly" >&2
    exit 1
}
FONT_FIXTURE=$TMP/font-contract.ttf
printf 'font-contract-fixture\n' >"$FONT_FIXTURE"
FONT_FIXTURE_SHA=$(sha256sum "$FONT_FIXTURE" | awk '{print $1}')
magicpaper_verify_font_sha256 "$FONT_FIXTURE_SHA" "$FONT_FIXTURE" fixture-font
if magicpaper_verify_font_sha256 "$MAGICPAPER_UI_FONT_SHA256" \
    "$FONT_FIXTURE" fixture-font >/dev/null 2>&1; then
    echo "MagicPaper font contract accepted the wrong content" >&2
    exit 1
fi
ln -s "$FONT_FIXTURE" "$FONT_FIXTURE.link"
if magicpaper_verify_font_sha256 "$FONT_FIXTURE_SHA" \
    "$FONT_FIXTURE.link" fixture-font >/dev/null 2>&1; then
    echo "MagicPaper font contract accepted a symbolic link" >&2
    exit 1
fi
if magicpaper_verify_font_sha256 invalid "$FONT_FIXTURE" \
    fixture-font >/dev/null 2>&1; then
    echo "MagicPaper font contract accepted an invalid expected digest" >&2
    exit 1
fi

for unit in \
    remagic-app@magicpaper.service remagic-app@koreader.service \
    remagic-display-host.service remagic-home.service riddle-takeover.service \
    riddle-legacy-ui.service 'appload@legacy.service'; do
    touch "$STATE/active/$unit" "$STATE/loaded/$unit"
done
stop_alternative_display_owners
for unit in \
    remagic-app@magicpaper.service remagic-app@koreader.service \
    remagic-display-host.service remagic-home.service riddle-takeover.service \
    riddle-legacy-ui.service 'appload@legacy.service'; do
    [ ! -e "$STATE/active/$unit" ] || { echo "unit was not stopped: $unit" >&2; exit 1; }
done

mkdir -p "$TMP/live" "$TMP/snapshots"
printf 'payload\n' >"$TMP/live/value"
ln -s value "$TMP/live/link"
snapshot_path "$TMP/snapshots" link "$TMP/live/link"
snapshot_record_is_complete "$TMP/snapshots" link "$TMP/live/link"
if snapshot_record_is_complete "$TMP/snapshots" link "$TMP/live/wrong"; then
    echo "snapshot record accepted the wrong source path" >&2
    exit 1
fi
rm -f "$TMP/live/link"
restore_snapshot "$TMP/snapshots" link
[ -L "$TMP/live/link" ] && [ "$(readlink "$TMP/live/link")" = value ]

snapshot_path "$TMP/snapshots" absent "$TMP/live/absent"
printf 'new\n' >"$TMP/live/absent"
restore_snapshot "$TMP/snapshots" absent
[ ! -e "$TMP/live/absent" ]

mkdir -p "$TMP/snapshots/incomplete"
printf '%s\n' "$TMP/live/value" >"$TMP/snapshots/incomplete/path"
if restore_snapshot "$TMP/snapshots" incomplete >/dev/null 2>&1; then
    echo "incomplete snapshot was restored" >&2
    exit 1
fi
[ "$(cat "$TMP/live/value")" = payload ]

# Fault both durability phases independently. Before publication, failure must
# leave no authoritative record. After publication, a reported failure may
# abort the install, but the already visible record must remain self-contained.
DEPLOYMENT_ORIGINAL_PATH=$PATH
DEPLOYMENT_SYNC_BIN=$TMP/sync-fault-bin
DEPLOYMENT_SYNC_COUNTER=$TMP/sync-counter
DEPLOYMENT_REAL_SYNC=$(command -v sync)
mkdir -p "$DEPLOYMENT_SYNC_BIN"
cat >"$DEPLOYMENT_SYNC_BIN/sync" <<EOF
#!/bin/sh
count=0
[ ! -r "\${REMAGIC_DEPLOYMENT_SYNC_COUNTER:-}" ] || \
    count=\$(sed -n '1p' "\$REMAGIC_DEPLOYMENT_SYNC_COUNTER")
count=\$((count + 1))
printf '%s\n' "\$count" >"\$REMAGIC_DEPLOYMENT_SYNC_COUNTER"
[ "\$count" -ne "\${REMAGIC_DEPLOYMENT_FAIL_SYNC_AT:-0}" ] || exit 71
exec "$DEPLOYMENT_REAL_SYNC" "\$@"
EOF
chmod 0755 "$DEPLOYMENT_SYNC_BIN/sync"
PATH=$DEPLOYMENT_SYNC_BIN:$PATH
export PATH REMAGIC_DEPLOYMENT_SYNC_COUNTER=$DEPLOYMENT_SYNC_COUNTER

printf '0\n' >"$DEPLOYMENT_SYNC_COUNTER"
REMAGIC_DEPLOYMENT_FAIL_SYNC_AT=1
export REMAGIC_DEPLOYMENT_FAIL_SYNC_AT
if snapshot_path "$TMP/snapshots" fault-before-publish "$TMP/live/value"; then
    echo "snapshot accepted a failed pre-publication durability barrier" >&2
    exit 1
fi
[ ! -e "$TMP/snapshots/fault-before-publish" ] || {
    echo "snapshot published a record after its pre-publication barrier failed" >&2
    exit 1
}

printf '0\n' >"$DEPLOYMENT_SYNC_COUNTER"
REMAGIC_DEPLOYMENT_FAIL_SYNC_AT=2
export REMAGIC_DEPLOYMENT_FAIL_SYNC_AT
if snapshot_path "$TMP/snapshots" fault-after-publish "$TMP/live/value"; then
    echo "snapshot hid a failed post-publication durability barrier" >&2
    exit 1
fi
snapshot_record_is_complete "$TMP/snapshots" fault-after-publish "$TMP/live/value" || {
    echo "published snapshot was incomplete after its final barrier failed" >&2
    exit 1
}
unset REMAGIC_DEPLOYMENT_FAIL_SYNC_AT REMAGIC_DEPLOYMENT_SYNC_COUNTER
PATH=$DEPLOYMENT_ORIGINAL_PATH
export PATH

mkdir -p "$TMP/snapshots/unsafe"
printf '%s\n' / >"$TMP/snapshots/unsafe/path"
: >"$TMP/snapshots/unsafe/complete"
if restore_snapshot "$TMP/snapshots" unsafe >/dev/null 2>&1; then
    echo "unsafe snapshot target was accepted" >&2
    exit 1
fi
canonical_absolute_path "$TMP/live/value"
if canonical_absolute_path "$TMP/live/../value"; then
    echo "non-canonical transaction path was accepted" >&2
    exit 1
fi

required_space=$(deployment_home_space_required 1000 200 50 300)
[ "$required_space" -eq 67286 ]
deployment_home_space_sufficient "$required_space" "$required_space"
if deployment_home_space_sufficient "$required_space" "$((required_space - 1))"; then
    echo "low-space preflight boundary was accepted" >&2
    exit 1
fi

# KOReader's preflight and migrator must consume one identical, de-duplicated
# set of legacy roots. Repeated paths and symlink aliases must not inflate the
# estimate, while every distinct real root must contribute its migratable data.
LEGACY_SPACE=$TMP/legacy-space
mkdir -p "$LEGACY_SPACE/one/settings" "$LEGACY_SPACE/two/clipboard" \
    "$LEGACY_SPACE/three/data/dict"
printf 'one\n' >"$LEGACY_SPACE/one/settings/value"
printf 'two\n' >"$LEGACY_SPACE/two/clipboard/value"
printf 'three\n' >"$LEGACY_SPACE/three/data/dict/value"
ln -s one "$LEGACY_SPACE/one-alias"
legacy_paths='settings clipboard data/dict'
one_kb=$(deployment_koreader_legacy_kb "$LEGACY_SPACE/one" "$legacy_paths")
two_kb=$(deployment_koreader_legacy_kb "$LEGACY_SPACE/two" "$legacy_paths")
three_kb=$(deployment_koreader_legacy_kb "$LEGACY_SPACE/three" "$legacy_paths")
all_kb=$(deployment_koreader_legacy_kb \
    "$LEGACY_SPACE/one:$LEGACY_SPACE/two:$LEGACY_SPACE/three:$LEGACY_SPACE/one:$LEGACY_SPACE/one-alias" \
    "$legacy_paths")
[ "$all_kb" -eq $((one_kb + two_kb + three_kb)) ] || {
    echo "KOReader legacy-space estimate did not cover exactly the unique real roots" >&2
    exit 1
}

# Managed writable roots are checked component by component. A link in an
# ancestor or anywhere in the active tree must be rejected before migration or
# the default settings redirection can write outside that tree.
SAFE_STORAGE=$TMP/safe-storage
OUTSIDE_STORAGE=$TMP/outside-storage
mkdir -p "$SAFE_STORAGE/data/settings" "$OUTSIDE_STORAGE"
deployment_assert_safe_storage_tree "$SAFE_STORAGE/data"
ln -s "$OUTSIDE_STORAGE" "$SAFE_STORAGE/data/settings/escape"
if deployment_assert_safe_storage_tree "$SAFE_STORAGE/data" >/dev/null 2>&1; then
    echo "managed KOReader data symlink was accepted" >&2
    exit 1
fi
rm "$SAFE_STORAGE/data/settings/escape"
ln -s "$OUTSIDE_STORAGE" "$SAFE_STORAGE/link"
if deployment_assert_safe_directory_path "$SAFE_STORAGE/link/data" >/dev/null 2>&1; then
    echo "intermediate KOReader storage symlink was accepted" >&2
    exit 1
fi

GUARD=$TMP/in-progress
GUARD_TXNS=$TMP/transactions
mkdir -p "$GUARD_TXNS/txn"
printf '%s\n' txn >"$GUARD"
printf '%s\n' preparing >"$GUARD_TXNS/txn/status"
deployment_guard_is_incomplete "$GUARD" "$GUARD_TXNS"
printf '%s\n' committed >"$GUARD_TXNS/txn/status"
if deployment_guard_is_incomplete "$GUARD" "$GUARD_TXNS"; then
    echo "committed deployment guard blocked recovery restart" >&2
    exit 1
fi
clear_deployment_guard_for_transaction "$GUARD" "$GUARD_TXNS/txn"
[ ! -e "$GUARD" ]

ORDERED_TXNS=$TMP/ordered-transactions
mkdir -p "$ORDERED_TXNS/20260721-090000-1" \
    "$ORDERED_TXNS/20260721-110000-3" "$ORDERED_TXNS/20260721-100000-2"
ordered_names=$(list_deployment_transactions_newest_first "$ORDERED_TXNS")
[ "$ordered_names" = "$(printf '%s\n' \
    20260721-110000-3 20260721-100000-2 20260721-090000-1)" ] || {
    echo "deployment transactions were not listed newest-first" >&2
    exit 1
}
mkdir "$ORDERED_TXNS/unsafe transaction"
if unsafe_names=$(list_deployment_transactions_newest_first "$ORDERED_TXNS" 2>/dev/null); then
    echo "unsafe deployment transaction name was accepted" >&2
    exit 1
fi
[ -z "$unsafe_names" ] || {
    echo "transaction listing emitted partial output before unsafe-name refusal" >&2
    exit 1
}

# A release rename must not make a journal produced by the previous release
# unrecoverable. This is the exact committed adapter path used by
# remagic-koreader before it became koreader-for-remagic.
LEGACY_FINISHED_TXN=$TMP/legacy-finished-transaction
mkdir -p "$LEGACY_FINISHED_TXN/switch-adapter" "$LEGACY_FINISHED_TXN/snapshots"
printf '%s\n' committed >"$LEGACY_FINISHED_TXN/status"
printf '%s\n' install >"$LEGACY_FINISHED_TXN/kind"
printf '%s\n' \
    "/home/root/apps/.remagic-koreader.rollback.deployment-safety-$$" \
    >"$LEGACY_FINISHED_TXN/switch-adapter/backup"
: >"$LEGACY_FINISHED_TXN/snapshots/fixture"
cleanup_finished_deployment_transaction "$LEGACY_FINISHED_TXN"
[ ! -e "$LEGACY_FINISHED_TXN/snapshots" ] || {
    echo "legacy committed adapter transaction was not retired" >&2
    exit 1
}

for deployment_script in \
    "$ROOT/scripts/install-device.sh" "$ROOT/scripts/uninstall-device.sh"; do
    grep -Fq \
        'adapter:/home/root/apps/remagic-koreader:/home/root/apps/.remagic-koreader.stage.*:/home/root/apps/.remagic-koreader.rollback.*' \
        "$deployment_script"
    grep -Fq \
        'adapter:/home/root/apps/remagic-koreader:/home/root/apps/.remagic-koreader.uninstall.*' \
        "$deployment_script"
done

acquire_directory_lock "$REMAGIC_INSTALL_LOCK" fixture
[ "$(sed -n '1p' "$REMAGIC_INSTALL_LOCK/pid")" = "$$" ]
release_directory_lock "$REMAGIC_INSTALL_LOCK"
[ ! -e "$REMAGIC_INSTALL_LOCK" ]

BARRIER=$TMP/recovery.lock
mkdir -p "$BARRIER"
printf '%s\n' 2147483647 >"$BARRIER/pid"
wait_for_lock_barrier "$BARRIER" fixture 1
[ ! -e "$BARRIER" ]
mkdir -p "$BARRIER"
printf '%s\n' "$$" >"$BARRIER/pid"
if wait_for_lock_barrier "$BARRIER" fixture 0 >/dev/null 2>&1; then
    echo "live recovery barrier was ignored" >&2
    exit 1
fi
rm -rf "$BARRIER"

daemon_dropin=$REMAGIC_SYSTEMD_RUNTIME_ROOT/remagicd.service.d/90-remagic-acceptance.conf
runner_dropin=$REMAGIC_SYSTEMD_RUNTIME_ROOT/remagic-app@.service.d/90-remagic-acceptance.conf
mkdir -p "$REMAGIC_ACCEPTANCE_LOCK" "$(dirname "$daemon_dropin")" "$(dirname "$runner_dropin")"
printf '%s\n' 2147483647 >"$REMAGIC_ACCEPTANCE_LOCK/pid"
: >"$daemon_dropin"
: >"$runner_dropin"
if cleanup_stale_acceptance_environment >/dev/null 2>&1; then
    echo "unproven stale acceptance state was deleted" >&2
    exit 1
fi
[ -e "$REMAGIC_ACCEPTANCE_LOCK" ] && [ -e "$daemon_dropin" ] && [ -e "$runner_dropin" ]
rm -rf "$REMAGIC_ACCEPTANCE_LOCK" "$(dirname "$daemon_dropin")" "$(dirname "$runner_dropin")"

REMAGIC_ACCEPTANCE_STALE_CLEANED=false
mkdir -p "$REMAGIC_ACCEPTANCE_LOCK" "$(dirname "$daemon_dropin")"
printf '%s\n' "$$" >"$REMAGIC_ACCEPTANCE_LOCK/pid"
: >"$daemon_dropin"
if cleanup_stale_acceptance_environment >/dev/null 2>&1; then
    echo "live acceptance lock was ignored" >&2
    exit 1
fi
[ -e "$daemon_dropin" ]
cleanup_stale_acceptance_environment allow-live
[ -e "$REMAGIC_ACCEPTANCE_LOCK" ] && [ -e "$daemon_dropin" ]
rm -rf "$REMAGIC_ACCEPTANCE_LOCK" "$(dirname "$daemon_dropin")"

mkdir -p "$(dirname "$runner_dropin")"
: >"$runner_dropin"
if cleanup_stale_acceptance_environment >/dev/null 2>&1; then
    echo "orphan acceptance override was removed without owner proof" >&2
    exit 1
fi
[ -e "$runner_dropin" ]
rm -rf "$(dirname "$runner_dropin")"

TREE=$TMP/adapter-switch
mkdir -p "$TREE/legacy/settings" "$TREE/data/settings" "$TREE/live" \
    "$TREE/stage/program/plugins" "$TREE/txn/snapshots"
printf 'independent-program\n' >"$TREE/legacy/reader.lua"
printf 'independent-session\n' >"$TREE/legacy/settings/session"
printf 'managed-adapter-old\n' >"$TREE/live/version"
printf 'active-session\n' >"$TREE/data/settings/session"
snapshot_path "$TREE/txn/snapshots" data "$TREE/data"
printf 'v2026.03\n' >"$TREE/stage/program/reader.lua"
printf 'v2026.03\n' >"$TREE/stage/program/git-rev"
transactional_tree_switch "$TREE/txn/switch-adapter" "$TREE/live" "$TREE/stage" "$TREE/backup"
[ "$(cat "$TREE/live/program/reader.lua")" = v2026.03 ]
[ "$(cat "$TREE/live/program/git-rev")" = v2026.03 ]
[ "$(cat "$TREE/legacy/reader.lua")" = independent-program ]
[ "$(cat "$TREE/legacy/settings/session")" = independent-session ]
[ "$(cat "$TREE/data/settings/session")" = active-session ]
[ ! -e "$TREE/live/program/update_once.marker" ]
[ ! -e "$TREE/live/program/plugins/terminal.koplugin" ]
printf 'mutated\n' >"$TREE/data/settings/session"
rollback_tree_switch "$TREE/txn/switch-adapter" "$TREE/live" "$TREE/stage" "$TREE/backup"
restore_snapshot "$TREE/txn/snapshots" data
[ "$(cat "$TREE/live/version")" = managed-adapter-old ]
[ "$(cat "$TREE/data/settings/session")" = active-session ]
[ "$(cat "$TREE/legacy/reader.lua")" = independent-program ]

rm -rf "$TREE/txn"
mkdir -p "$TREE/stage/program/plugins" "$TREE/txn"
printf 'v2026.03\n' >"$TREE/stage/program/reader.lua"
printf 'v2026.03\n' >"$TREE/stage/program/git-rev"
transactional_tree_switch "$TREE/txn/switch-adapter" "$TREE/live" "$TREE/stage" "$TREE/backup"
rm -rf "$TREE/backup"
[ "$(cat "$TREE/live/program/reader.lua")" = v2026.03 ]
[ "$(cat "$TREE/legacy/reader.lua")" = independent-program ]
[ "$(cat "$TREE/data/settings/session")" = active-session ]

ELF=$TMP/aarch64.elf
printf '\177ELF\002\001\001\000\000\000\000\000\000\000\000\000\000\000\267\000' >"$ELF"
assert_aarch64_elf "$ELF"
if assert_aarch64_elf /bin/sh >/dev/null 2>&1; then
    echo "x86 host shell was accepted as AArch64" >&2
    exit 1
fi

# The supported installer is the split system release. It verifies every file
# before touching display ownership, snapshots all system-owned publication
# points, atomically swaps only the ReMagic tree, then installs Store through
# the same content-addressed package manager used for updates.
SYSTEM_INSTALL=$ROOT/scripts/system-release/install-device.sh
SYSTEM_BUILD=$ROOT/scripts/build-system-release.sh
sh -n "$ROOT/install.sh"
sh -n "$SYSTEM_INSTALL"
bash -n "$SYSTEM_BUILD"
grep -Fq '(cd "$SOURCE_DIR" && sha256sum -c SHA256SUMS >/dev/null)' "$SYSTEM_INSTALL"
grep -Fq '(cd "$PAYLOAD" && sha256sum -c share/system-files.sha256 >/dev/null)' "$SYSTEM_INSTALL"
grep -Fq 'snapshot_path "$path" "$name"' "$SYSTEM_INSTALL"
grep -Fq 'mv "$APP_ROOT" "$BACKUP"' "$SYSTEM_INSTALL"
grep -Fq 'mv "$STAGE" "$APP_ROOT"' "$SYSTEM_INSTALL"
grep -Fq 'remagic-package-inspect" install "$STORE_BUNDLE"' "$SYSTEM_INSTALL"
grep -Fq 'STORE_ACCEPTED_REVISION=/home/root/.local/state/remagic-store/accepted-revision' \
    "$SYSTEM_INSTALL"
grep -Fq 'if [ -e "$STORE_ACCEPTED_REVISION" ]; then' "$SYSTEM_INSTALL"
grep -Fq 'wait_for_remagic_ready "$APP_ROOT/bin/remagicctl"' "$SYSTEM_INSTALL"
grep -Fq 'installation failed; restoring the previous system' "$SYSTEM_INSTALL"
grep -Fq 'restore_stock' "$SYSTEM_INSTALL"
agent_stop_line=$(grep -n '^    systemctl stop magicpaper-agent.service' \
    "$SYSTEM_INSTALL" | cut -d: -f1)
core_switch_line=$(grep -n '^    mv "$APP_ROOT" "$BACKUP"' \
    "$SYSTEM_INSTALL" | tail -1 | cut -d: -f1)
legacy_remove_line=$(grep -n '^rm -f /run/systemd/system/magicpaper-agent.service' \
    "$SYSTEM_INSTALL" | cut -d: -f1)
register_line=$(grep -n '^"$APP_ROOT/libexec/remagic-register" --persistent' \
    "$SYSTEM_INSTALL" | cut -d: -f1)
[ "$agent_stop_line" -lt "$core_switch_line" ] && \
    [ "$legacy_remove_line" -lt "$register_line" ] || {
    echo "retired MagicPaper agent is not stopped and unlinked before system publication" >&2
    exit 1
}

# User applications are separate packages. The system release must not embed
# either payload, delete their state, or expose the adapter repository name as
# KOReader's installed display label.
if grep -Eq 'opt/magicpaper|opt/koreader-for-remagic|MAGICPAPER_DIR' "$SYSTEM_BUILD"; then
    echo "system release embeds a user application payload" >&2
    exit 1
fi
if grep -Eq 'rm -rf .*(/home/root/books|\.local/share/koreader-for-remagic|\.local/share/magicpaper)' \
    "$SYSTEM_INSTALL"; then
    echo "system installer can delete user books or application data" >&2
    exit 1
fi
grep -Fxq 'name = "KOReader"' "$ROOT/testing/manifests/koreader.toml"
! grep -Eq '^name = ".*(for ReMagic|验收环境)' "$ROOT/testing/manifests/koreader.toml"
grep -Fq 'ReadWritePaths=/home/root/.local/share/koreader-for-remagic' \
    "$ROOT/systemd/remagic-app@koreader.service.d/10-koreader-runtime.conf"
grep -Fq 'ReadOnlyPaths=/home/root' \
    "$ROOT/systemd/remagic-app@koreader.service.d/10-koreader-runtime.conf"
grep -q '^Conflicts=.*riddle-takeover.service' "$ROOT/systemd/remagicd.service"
grep -q '^Conflicts=.*magicpaper-takeover.service' "$ROOT/systemd/remagicd.service"
grep -q '^Conflicts=.*magicpaper-power-launcher.service' "$ROOT/systemd/remagicd.service"
grep -q '^Environment=REMAGIC_PLATFORM_CAPABILITIES=.*input:mode-v2' \
    "$ROOT/systemd/remagic-app@.service" || {
    echo "application service omitted the dynamic input-mode capability" >&2
    exit 1
}
grep -q '^Environment=REMAGIC_PLATFORM_CAPABILITIES=.*agent:pi-v1' \
    "$ROOT/systemd/remagic-app@.service" || {
    echo "application service omitted the Pi agent capability" >&2
    exit 1
}
grep -q '^Environment=REMAGIC_PLATFORM_CAPABILITIES=.*network:lan-peer-v1' \
    "$ROOT/systemd/remagic-app@.service" || {
    echo "application service omitted the LAN peer capability" >&2
    exit 1
}
grep -q '^Environment=REMAGIC_PLATFORM_CAPABILITIES=.*sync:koreader-state-v1' \
    "$ROOT/systemd/remagic-app@.service" || {
    echo "application service omitted the KOReader sync capability" >&2
    exit 1
}
grep -q '^Environment=REMAGIC_AGENT_SOCKET=/run/remagic/agent.sock' \
    "$ROOT/systemd/remagic-app@.service" || {
    echo "application service omitted the Pi agent socket" >&2
    exit 1
}
grep -q '^Wants=remagic-agentd.socket$' "$ROOT/systemd/remagic-app@.service" || {
    echo "application service does not request the optional Pi socket" >&2
    exit 1
}
if grep -q '^Requires=.*remagic-agentd.socket' "$ROOT/systemd/remagic-app@.service"; then
    echo "all applications incorrectly depend on the optional Pi socket" >&2
    exit 1
fi
shutdown_max_ms=$(sed -n \
    's/^pub const MAX_SHUTDOWN_KILL_TIMEOUT_MS: u64 = \([0-9_][0-9_]*\);$/\1/p' \
    "$ROOT/crates/remagic-core/src/manifest.rs" | tr -d _)
unit_stop_seconds=$(sed -n 's/^TimeoutStopSec=\([0-9][0-9]*\)$/\1/p' \
    "$ROOT/systemd/remagic-app@.service")
[ -n "$shutdown_max_ms" ] && [ -n "$unit_stop_seconds" ] && \
    [ $((unit_stop_seconds * 1000 - shutdown_max_ms)) -ge 2000 ] || {
    echo "application shutdown budget has less than two seconds of systemd margin" >&2
    exit 1
}

echo "deployment safety fixture passed"
