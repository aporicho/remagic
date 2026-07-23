#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

fail() {
    echo "device-test-isolation fixture: $*" >&2
    exit 1
}

mkdir -p "$TMP/bin" "$TMP/manifests" "$TMP/templates" "$TMP/protected-a" "$TMP/protected-b"
mkdir -p "$TMP/sessions"
magic_content=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
koreader_content=bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
mkdir -p "$TMP/apps/magicpaper/releases/$magic_content/payload" \
    "$TMP/apps/koreader/releases/$koreader_content/payload/adapter/releases/adapter-test" \
    "$TMP/apps/koreader/releases/$koreader_content/payload/vendor/releases/vendor-test/koreader"
ln -s "releases/$magic_content" "$TMP/apps/magicpaper/current"
ln -s "releases/$koreader_content" "$TMP/apps/koreader/current"
printf '%s\n' \
    'name = "MagicPaper"' \
    "working_dir = \"$TMP/apps/magicpaper/current/payload\"" \
    >"$TMP/manifests/magicpaper.toml"
printf '%s\n' \
    'name = "KOReader"' \
    "exec = \"$TMP/apps/koreader/current/payload/adapter/releases/adapter-test/bin/koreader-for-remagic\"" \
    "working_dir = \"$TMP/apps/koreader/current/payload/vendor/releases/vendor-test/koreader\"" \
    >"$TMP/manifests/koreader.toml"
printf '%s\n' 'test magicpaper' \
    'exec = "/__REMAGIC_MAGICPAPER_PAYLOAD_ROOT__/bin/magicpaper-launch"' \
    'working_dir = "/__REMAGIC_MAGICPAPER_PAYLOAD_ROOT__"' \
    >"$TMP/templates/magicpaper.toml"
printf '%s\n' 'test koreader' \
    'name = "KOReader"' \
    'exec = "/__REMAGIC_KOREADER_ADAPTER_ROOT__/bin/koreader-for-remagic"' \
    'working_dir = "/__REMAGIC_KOREADER_VENDOR_ROOT__"' \
    'migrator = "/__REMAGIC_KOREADER_ADAPTER_ROOT__/libexec/koreader-data-migrate"' \
    'KOREADER_LIBRARY_STATE_ROOT = "/home/root/.local/state/remagic/acceptance/current/koreader/library-state"' \
    'KOREADER_SOURCE_LIBRARY_DIR = "/home/root/.local/state/remagic/acceptance/current/koreader/source-library"' \
    >"$TMP/templates/koreader.toml"
printf 'untouched a\n' >"$TMP/protected-a/data"
printf 'untouched b\n' >"$TMP/protected-b/data"
printf 'production session\n' >"$TMP/sessions/magicpaper.json"

cat >"$TMP/bin/remagicctl" <<'EOF'
#!/bin/sh
case ${1-} in
    status) printf '%s\n' '{"domain": "system"}' ;;
    system) ;;
    *) exit 2 ;;
esac
EOF
cat >"$TMP/bin/systemctl" <<'EOF'
#!/bin/sh
[ -z "${REMAGIC_FIXTURE_SYSTEMCTL_LOG:-}" ] || printf '%s\n' "$*" >>"$REMAGIC_FIXTURE_SYSTEMCTL_LOG"
case ${1-}:${2-} in
    is-active:xochitl.service|is-active:remagicd.service) printf '%s\n' active ;;
    is-active:*) printf '%s\n' inactive ;;
    restart:*|stop:*|start:*|daemon-reload:*) ;;
    show:remagic-app@magicpaper.service) printf '%s\n' "${REMAGIC_FIXTURE_IP_DENY:-}" ;;
    show:paperweight.service) printf '%s\n' "${REMAGIC_FIXTURE_PAPERWEIGHT_LOAD:-not-found}" ;;
    *) exit 2 ;;
esac
EOF
chmod 0755 "$TMP/bin/remagicctl" "$TMP/bin/systemctl"

export REMAGIC_TEST_ALLOW_NON_ROOT=1
export REMAGIC_TEST_LOCK=$TMP/acceptance.lock
export REMAGIC_INSTALL_LOCK=$TMP/install.lock
export REMAGIC_TEST_ROOT=$TMP/test-root
export REMAGIC_APP_ROOT=$TMP/app-root
export REMAGIC_TEST_TEMPLATES=$TMP/templates
export REMAGIC_TEST_PRODUCTION_MANIFEST_ROOT=$TMP/manifests
export REMAGIC_TEST_APPS_ROOT=$TMP/apps
export REMAGIC_TEST_CTL=$TMP/bin/remagicctl
export REMAGIC_TEST_SYSTEMCTL=$TMP/bin/systemctl
export REMAGIC_TEST_SYSTEMD_RUNTIME_ROOT=$TMP/systemd
export REMAGIC_TEST_SESSION_ROOT=$TMP/sessions
export REMAGIC_TEST_PROTECTED_PATHS=$TMP/protected-a:$TMP/protected-b
export REMAGIC_TEST_RECOVERY_HELPER=$ROOT/scripts/lib/device-test-recovery.sh
export REMAGIC_TEST_MANIFEST_HELPER=$ROOT/scripts/lib/device-test-manifests.sh
export REMAGIC_FIXTURE_SYSTEMCTL_LOG=$TMP/systemctl.log
export REMAGIC_FIXTURE_IP_DENY=any
export REMAGIC_FIXTURE_PAPERWEIGHT_LOAD=not-found

# shellcheck source=../scripts/lib/device-test-isolation.sh
. "$ROOT/scripts/lib/device-test-isolation.sh"

# Sourced helpers must not overwrite caller state, even when remagicctl returns
# a JSON document that is not a valid shell exit status.
status=caller-status
attempts=caller-attempts
unit=caller-unit
acceptance_state=caller-acceptance-state
remagic_test_wait_stock || fail 'stock wait failed during caller-scope regression'
[ "$status" = caller-status ] || fail 'wait_stock overwrote caller status'
[ "$attempts" = caller-attempts ] || fail 'wait_stock overwrote caller attempts'
[ "$unit" = caller-unit ] || fail 'wait_stock overwrote caller unit'
remagic_acceptance_safe_path "$TMP" || fail 'safe path rejected fixture root'
[ "$acceptance_state" = caller-acceptance-state ] || fail 'recovery helper polluted caller state'

# All display and lifecycle counters are Rust u64 values. Exercise boundaries
# above signed i64 and above awk's exact-double range without shell arithmetic.
[ "$(remagic_test_u64_canonical 00042)" = 42 ] || fail 'u64 canonicalization failed'
remagic_test_u64_nonzero 18446744073709551615 || fail 'u64 max was rejected as nonzero'
remagic_test_u64_greater 18446744073709551615 9223372036854775807 || fail 'u64 max comparison failed'
remagic_test_u64_greater 9007199254740993 9007199254740992 || fail 'post-double u64 comparison failed'
remagic_test_u64_is_next 9007199254740992 9007199254740993 || fail 'post-double increment failed'
remagic_test_u64_is_next 999999999999999999 1000000000000000000 || fail 'u64 carry increment failed'
if remagic_test_u64_canonical 18446744073709551616 >/dev/null 2>&1; then
    fail 'out-of-range u64 was accepted'
fi
if remagic_test_u64_next_value 18446744073709551615 >/dev/null 2>&1; then
    fail 'u64 max increment wrapped'
fi

for acceptance_script in \
    "$ROOT/scripts/device-acceptance-v2.sh" \
    "$ROOT/scripts/device-fault-acceptance-v2.sh" \
    "$ROOT/scripts/device-stress-acceptance-v2.sh"; do
    grep -Fq 'trap '\''cleanup "$?"'\'' EXIT' "$acceptance_script" || \
        fail "EXIT trap does not pass its status: $acceptance_script"
done

remagic_test_begin fixture
[ ! -e "$(remagic_acceptance_prepare_stage_path "$$")" ] || fail 'journal staging directory leaked after atomic publication'
grep -q '^name = "MagicPaper"$' "$TMP/manifests/magicpaper.toml" || fail 'production manifest was overwritten'
grep -q '^test magicpaper$' "$REMAGIC_TEST_ROOT/manifests/magicpaper.toml" || fail 'test manifest was not staged'
grep -q "^working_dir = \"$TMP/apps/magicpaper/releases/$magic_content/payload\"$" \
    "$REMAGIC_TEST_ROOT/manifests/magicpaper.toml" || fail 'MagicPaper release path was not materialized'
grep -q "REMAGIC_MANIFEST_ROOT=$REMAGIC_TEST_ROOT/manifests" \
    "$TMP/systemd/remagicd.service.d/90-remagic-acceptance.conf" || fail 'daemon override was not installed'
grep -q "REMAGIC_WELCOME_MARKER=$REMAGIC_TEST_ROOT/welcome-v1" \
    "$TMP/systemd/remagic-home.service.d/90-remagic-acceptance.conf" || fail 'Home welcome state was not isolated'
[ "$(cat "$REMAGIC_TEST_ROOT/welcome-v1")" = completed ] || fail 'isolated Home was not placed past first run'
grep -q '^IPAddressDeny=any$' \
    "$TMP/systemd/remagic-app@.service.d/90-remagic-acceptance.conf" || fail 'OS network deny was not installed'
[ "$(stat -c %a "$TMP/systemd/remagicd.service.d/90-remagic-acceptance.conf")" = 644 ] || fail 'daemon drop-in mode is not 0644'
[ "$(stat -c %a "$TMP/systemd/remagic-home.service.d/90-remagic-acceptance.conf")" = 644 ] || fail 'Home drop-in mode is not 0644'
[ "$(stat -c %a "$TMP/systemd/remagic-app@.service.d/90-remagic-acceptance.conf")" = 644 ] || fail 'runner drop-in mode is not 0644'
[ ! -e "$(remagic_acceptance_override_temp_path remagicd.service)" ] || fail 'daemon drop-in staging file leaked'
[ ! -e "$(remagic_acceptance_override_temp_path remagic-home.service)" ] || fail 'Home drop-in staging file leaked'
[ ! -e "$(remagic_acceptance_override_temp_path 'remagic-app@.service')" ] || fail 'runner drop-in staging file leaked'
grep -q "KOREADER_LIBRARY_STATE_ROOT = \"$REMAGIC_TEST_ROOT/koreader/library-state\"" \
    "$REMAGIC_TEST_ROOT/manifests/koreader.toml" || fail 'KOReader library state was not isolated'
grep -q "KOREADER_SOURCE_LIBRARY_DIR = \"$REMAGIC_TEST_ROOT/koreader/source-library\"" \
    "$REMAGIC_TEST_ROOT/manifests/koreader.toml" || fail 'KOReader source library was not isolated'
grep -q '^name = "KOReader"$' "$REMAGIC_TEST_ROOT/manifests/koreader.toml" || \
    fail 'KOReader visible name changed during test materialization'
grep -q "^exec = \"$TMP/apps/koreader/releases/$koreader_content/payload/adapter/releases/adapter-test/bin/koreader-for-remagic\"$" \
    "$REMAGIC_TEST_ROOT/manifests/koreader.toml" || fail 'KOReader adapter path was not materialized'
grep -q "^working_dir = \"$TMP/apps/koreader/releases/$koreader_content/payload/vendor/releases/vendor-test/koreader\"$" \
    "$REMAGIC_TEST_ROOT/manifests/koreader.toml" || fail 'KOReader vendor path was not materialized'
if grep -q '__REMAGIC_' "$REMAGIC_TEST_ROOT/manifests/koreader.toml"; then
    fail 'KOReader materialization left a placeholder behind'
fi
printf 'disposable\n' >"$REMAGIC_TEST_ROOT/magicpaper/data/test-output"
printf 'test session\n' >"$TMP/sessions/magicpaper.json"
remagic_test_finish || fail 'clean isolated run did not finish'
grep -q '^name = "MagicPaper"$' "$TMP/manifests/magicpaper.toml" || fail 'production manifest was not restored'
grep -q '^name = "KOReader"$' "$TMP/manifests/koreader.toml" || fail 'second production manifest was not restored'
grep -q "^exec = \"$TMP/apps/koreader/current/payload/adapter/releases/adapter-test/bin/koreader-for-remagic\"$" \
    "$TMP/manifests/koreader.toml" || fail 'production KOReader executable changed'
[ ! -e "$TMP/systemd/remagicd.service.d/90-remagic-acceptance.conf" ] || fail 'daemon override leaked'
[ ! -e "$TMP/systemd/remagic-home.service.d/90-remagic-acceptance.conf" ] || fail 'Home override leaked'
[ ! -e "$TMP/systemd/remagic-app@.service.d/90-remagic-acceptance.conf" ] || fail 'runner override leaked'
[ ! -e "$REMAGIC_TEST_ROOT" ] || fail 'successful disposable root was retained'
[ ! -e "$REMAGIC_TEST_LOCK" ] || fail 'successful run leaked its lock'
grep -q '^production session$' "$TMP/sessions/magicpaper.json" || fail 'production manager session was not restored'

REMAGIC_FIXTURE_IP_DENY=
if remagic_test_begin unsupported-network 2>"$TMP/network.log"; then
    fail 'missing systemd network isolation was silently accepted'
fi
remagic_test_finish || fail 'network-isolation refusal could not restore infrastructure'
grep -q 'IPAddressDeny=any is unavailable' "$TMP/network.log" || fail 'network-isolation diagnostic is missing'
[ ! -e "$TMP/systemd/remagic-app@.service.d/90-remagic-acceptance.conf" ] || fail 'failed network setup leaked a drop-in'
REMAGIC_FIXTURE_IP_DENY=any

reserved=$TMP/systemd/remagic-app@.service.d/90-remagic-acceptance.conf
mkdir -p "$(dirname "$reserved")"
printf '%s\n' '[Service]' 'Environment=FOREIGN=1' >"$reserved"
if remagic_test_begin reserved-name 2>"$TMP/reserved.log"; then
    fail 'pre-existing reserved drop-in was overwritten'
fi
remagic_test_finish || fail 'reserved-name refusal did not release its untouched lock'
grep -q 'reserved drop-in already exists' "$TMP/reserved.log" || fail 'reserved-name diagnostic is missing'
grep -q 'Environment=FOREIGN=1' "$reserved" || fail 'foreign reserved drop-in was deleted'
rm -f "$reserved"
rmdir "$(dirname "$reserved")"

remagic_test_begin mutation-check
printf 'forbidden mutation\n' >>"$TMP/protected-a/data"
if remagic_test_finish 2>"$TMP/mutation.log"; then
    fail 'protected production mutation was not detected'
fi
grep -q 'protected production data changed' "$TMP/mutation.log" || fail 'mutation diagnostic is missing'
diagnostic_root=$(find "$REMAGIC_TEST_DIAGNOSTICS_ROOT" -mindepth 1 -maxdepth 1 -type d -print -quit)
[ -s "$diagnostic_root/production.before.sha256" ] || fail 'failed run lost before fingerprint'
[ -s "$diagnostic_root/production.after.sha256" ] || fail 'failed run lost after fingerprint'
[ ! -e "$REMAGIC_TEST_ROOT" ] || fail 'failed run left the active transaction occupied'
[ ! -e "$REMAGIC_TEST_LOCK" ] || fail 'failed run leaked its lock'
printf 'untouched a\n' >"$TMP/protected-a/data"
remagic_test_begin after-diagnostic
remagic_test_finish || fail 'a diagnostic run blocked the next acceptance run'

simulate_interrupted_run() {
    phase=$1
    printf 'production session\n' >"$TMP/sessions/magicpaper.json"
    : >"$REMAGIC_FIXTURE_SYSTEMCTL_LOG"
    remagic_test_lock || fail "could not claim setup lock for $phase"
    remagic_acceptance_prepare "orphan-$phase" "$$" || fail "could not journal $phase"
    REMAGIC_TEST_TRANSACTION_PREPARED=true
    remagic_test_seed_data
    remagic_acceptance_save_sessions
    : >"$REMAGIC_TEST_ROOT/transaction/agent-was-active"
    case $phase in
        sessions-saved) ;;
        overrides-installing)
            remagic_acceptance_set_state overrides-installing
            partial=$TMP/systemd/remagicd.service.d/90-remagic-acceptance.conf
            mkdir -p "$(dirname "$partial")"
            printf '%s\n' '[Service]' \
                "Environment=REMAGIC_MANIFEST_ROOT=$REMAGIC_TEST_ROOT/manifests" >"$partial"
            ;;
        override-staged)
            remagic_acceptance_set_state overrides-installing
            partial=$(remagic_acceptance_override_temp_path remagicd.service)
            mkdir -p "$(dirname "$partial")"
            printf '%s\n' '[Service]' 'Environment=REMAGIC_MANIFEST_' >"$partial"
            ;;
        overrides-installed)
            remagic_acceptance_set_state overrides-installing
            remagic_test_install_overrides || fail 'could not create installed override fixture'
            remagic_acceptance_set_state overrides-installed
            ;;
        *) fail "unknown interrupted phase: $phase" ;;
    esac
    printf 'mutated by interrupted test\n' >"$TMP/sessions/magicpaper.json"
    dead_owner=2147483647
    printf '%s\n' "$dead_owner" >"$REMAGIC_TEST_LOCK/pid"
    printf '%s\n' "$dead_owner" >"$REMAGIC_TEST_ROOT/transaction/owner-pid"
    sync
    REMAGIC_TEST_LOCKED=false
    REMAGIC_TEST_SESSIONS_SAVED=false
    REMAGIC_TEST_OVERRIDES_INSTALLED=false
    REMAGIC_TEST_TRANSACTION_PREPARED=false

    remagic_test_lock || fail "orphan recovery failed for $phase"
    grep -q '^production session$' "$TMP/sessions/magicpaper.json" || fail "$phase lost the production session"
    [ ! -e "$TMP/systemd/remagicd.service.d/90-remagic-acceptance.conf" ] || fail "$phase leaked daemon drop-in"
    [ ! -e "$TMP/systemd/remagic-home.service.d/90-remagic-acceptance.conf" ] || fail "$phase leaked Home drop-in"
    [ ! -e "$TMP/systemd/remagic-app@.service.d/90-remagic-acceptance.conf" ] || fail "$phase leaked runner drop-in"
    [ ! -e "$(remagic_acceptance_override_temp_path remagicd.service)" ] || fail "$phase leaked daemon drop-in staging"
    [ ! -e "$(remagic_acceptance_override_temp_path remagic-home.service)" ] || fail "$phase leaked Home drop-in staging"
    [ ! -e "$(remagic_acceptance_override_temp_path 'remagic-app@.service')" ] || fail "$phase leaked runner drop-in staging"
    grep -q '^start magicpaper-agent.service$' "$REMAGIC_FIXTURE_SYSTEMCTL_LOG" || fail "$phase did not restore the agent"
    [ ! -e "$REMAGIC_TEST_ROOT" ] || fail "$phase retained its recovered current root"
    remagic_test_release_lock
    REMAGIC_TEST_AGENT_WAS_ACTIVE=false
}

for interrupted_phase in sessions-saved overrides-installing override-staged overrides-installed; do
    simulate_interrupted_run "$interrupted_phase"
done

# A crash after the terminal state and durable test-root deletion leaves only
# the lock journal. Both normal completion and recovery completion must be
# reclaimable, and an optional Paperweight unit must never make recovery fail.
simulate_terminal_without_journal() {
    terminal_state=$1
    paperweight_load=$2
    : >"$REMAGIC_FIXTURE_SYSTEMCTL_LOG"
    REMAGIC_FIXTURE_PAPERWEIGHT_LOAD=$paperweight_load
    export REMAGIC_FIXTURE_PAPERWEIGHT_LOAD
    remagic_test_lock || fail "could not claim terminal lock for $terminal_state"
    remagic_acceptance_prepare "terminal-$terminal_state" "$$" || \
        fail "could not prepare terminal transaction for $terminal_state"
    REMAGIC_TEST_TRANSACTION_PREPARED=true
    remagic_acceptance_set_state "$terminal_state" || \
        fail "could not publish terminal state $terminal_state"
    dead_owner=2147483647
    printf '%s\n' "$dead_owner" >"$REMAGIC_TEST_LOCK/pid"
    printf '%s\n' "$dead_owner" >"$REMAGIC_TEST_ROOT/transaction/owner-pid"
    remagic_acceptance_remove_test_root || \
        fail "could not stage no-journal $terminal_state fixture"
    REMAGIC_TEST_LOCKED=false
    REMAGIC_TEST_TRANSACTION_PREPARED=false

    remagic_test_lock || fail "no-journal $terminal_state recovery failed"
    [ ! -e "$REMAGIC_TEST_ROOT" ] || fail "$terminal_state recovery recreated the test root"
    case $paperweight_load in
        loaded)
            grep -q '^start paperweight.service$' "$REMAGIC_FIXTURE_SYSTEMCTL_LOG" || \
                fail 'loaded Paperweight was not restored'
            ;;
        *)
            if grep -q '^start paperweight.service$' "$REMAGIC_FIXTURE_SYSTEMCTL_LOG"; then
                fail 'missing optional Paperweight was started'
            fi
            ;;
    esac
    remagic_test_release_lock || fail "could not release post-$terminal_state lock"
}

simulate_terminal_without_journal finished not-found
simulate_terminal_without_journal recovered loaded
REMAGIC_FIXTURE_PAPERWEIGHT_LOAD=not-found
export REMAGIC_FIXTURE_PAPERWEIGHT_LOAD

# The recursive delete itself may be interrupted, leaving an invalid partial
# transaction directory. The already durable terminal lock is sufficient proof
# that recovery may finish deleting it without trusting partial journal files.
remagic_test_lock || fail 'could not claim partial-terminal lock'
remagic_acceptance_prepare partial-terminal "$$" || fail 'could not prepare partial-terminal transaction'
REMAGIC_TEST_TRANSACTION_PREPARED=true
remagic_acceptance_set_state finished || fail 'could not publish partial-terminal state'
dead_owner=2147483647
printf '%s\n' "$dead_owner" >"$REMAGIC_TEST_LOCK/pid"
printf '%s\n' "$dead_owner" >"$REMAGIC_TEST_ROOT/transaction/owner-pid"
rm -f "$REMAGIC_TEST_ROOT/transaction/format"
sync
REMAGIC_TEST_LOCKED=false
REMAGIC_TEST_TRANSACTION_PREPARED=false
remagic_test_lock || fail 'partial terminal-root recovery failed'
[ ! -e "$REMAGIC_TEST_ROOT" ] || fail 'partial terminal-root recovery retained test data'
remagic_test_release_lock || fail 'could not release post-partial-terminal lock'

ORIGINAL_PATH=$PATH
FAIL_BIN=$TMP/fail-bin
mkdir -p "$FAIL_BIN"
make_failure_wrapper() {
    wrapper_name=$1
    wrapper_real=$(PATH=$ORIGINAL_PATH command -v "$wrapper_name")
    [ -n "$wrapper_real" ] || fail "missing real command for fault injection: $wrapper_name"
    {
        printf '%s\n' '#!/bin/sh'
        printf '%s\n' "[ \"\${REMAGIC_FIXTURE_FAIL_COMMAND:-}\" != '$wrapper_name' ] || exit 71"
        printf 'exec %s "$@"\n' "$wrapper_real"
    } >"$FAIL_BIN/$wrapper_name"
    chmod 0755 "$FAIL_BIN/$wrapper_name"
}
for wrapped_command in cp find mv rm sha256sum sync; do
    make_failure_wrapper "$wrapped_command"
done
PATH=$FAIL_BIN:$ORIGINAL_PATH
export PATH
# dash retains command lookups performed before PATH changes. Clear that cache
# so every injected failure is exercised through the wrapper on every shell.
hash -r 2>/dev/null || true

# Lock ownership files remain intact if the atomic retirement rename fails.
remagic_test_lock || fail 'could not claim lock-retirement fault fixture'
REMAGIC_FIXTURE_FAIL_COMMAND=mv
export REMAGIC_FIXTURE_FAIL_COMMAND
if remagic_test_release_lock >/dev/null 2>&1; then
    fail 'lock release accepted a failed atomic retirement rename'
fi
unset REMAGIC_FIXTURE_FAIL_COMMAND
[ "$(sed -n '1p' "$REMAGIC_TEST_LOCK/pid")" = "$$" ] || \
    fail 'failed lock retirement destroyed ownership evidence'
remagic_test_release_lock || fail 'lock retirement could not retry after rename failure'

for producer_failure in find sha256sum; do
    if (
        REMAGIC_FIXTURE_FAIL_COMMAND=$producer_failure
        export REMAGIC_FIXTURE_FAIL_COMMAND
        remagic_test_fingerprint >/dev/null 2>&1
    ); then
        fail "$producer_failure failure was hidden by fingerprint generation"
    fi
done

remagic_test_lock || fail 'could not lock copy-failure transaction'
remagic_acceptance_prepare copy-failure "$$" || fail 'could not prepare copy-failure transaction'
REMAGIC_TEST_TRANSACTION_PREPARED=true
remagic_test_seed_data || fail 'could not seed copy-failure transaction'
REMAGIC_FIXTURE_FAIL_COMMAND=cp
export REMAGIC_FIXTURE_FAIL_COMMAND
if remagic_acceptance_save_sessions; then
    fail 'session snapshot accepted a failed cp'
fi
unset REMAGIC_FIXTURE_FAIL_COMMAND
[ -e "$REMAGIC_TEST_LOCK" ] || fail 'copy failure dropped the recovery lock'
[ ! -e "$REMAGIC_TEST_ROOT/original-manager-sessions/complete" ] || \
    fail 'failed copy published a complete session snapshot'
remagic_test_finish || fail 'copy-failure transaction could not cleanly abort'

remagic_test_begin restore-mv-failure
printf 'test session after begin\n' >"$TMP/sessions/magicpaper.json"
REMAGIC_FIXTURE_FAIL_COMMAND=mv
export REMAGIC_FIXTURE_FAIL_COMMAND
if remagic_test_finish >/dev/null 2>&1; then
    fail 'session restore accepted a failed mv'
fi
unset REMAGIC_FIXTURE_FAIL_COMMAND
[ -e "$REMAGIC_TEST_LOCK" ] || fail 'restore failure released the recovery lock'
[ -e "$REMAGIC_TEST_ROOT/transaction" ] || fail 'restore failure discarded its transaction journal'
remagic_acceptance_recover_orphan "$$" || fail 'restore failure was not recoverable'
REMAGIC_TEST_LOCKED=false
grep -q '^production session$' "$TMP/sessions/magicpaper.json" || \
    fail 'recovery after mv failure lost the production session'

remagic_test_lock || fail 'could not lock rm-failure transaction'
remagic_acceptance_prepare rm-failure "$$" || fail 'could not prepare rm-failure transaction'
REMAGIC_TEST_TRANSACTION_PREPARED=true
remagic_acceptance_set_state finished || fail 'could not mark rm-failure transaction finished'
rm -rf "$REMAGIC_TEST_ROOT/transaction" || fail 'could not stage finished-without-journal recovery'
REMAGIC_FIXTURE_FAIL_COMMAND=rm
export REMAGIC_FIXTURE_FAIL_COMMAND
if remagic_acceptance_recover_orphan "$$" >/dev/null 2>&1; then
    fail 'orphan recovery accepted a failed transaction rm'
fi
unset REMAGIC_FIXTURE_FAIL_COMMAND
[ -e "$REMAGIC_TEST_LOCK" ] || fail 'failed transaction rm released the recovery lock'
[ -e "$REMAGIC_TEST_ROOT" ] || fail 'failed transaction rm lost the recovery journal'
remagic_acceptance_recover_orphan "$$" || fail 'rm-failure transaction was not recoverable'
REMAGIC_TEST_LOCKED=false

# A failed durability barrier after the root was deleted must retain the
# terminal lock. Retrying recovery then follows the no-journal path above.
remagic_test_lock || fail 'could not lock sync-failure transaction'
remagic_acceptance_prepare sync-failure "$$" || fail 'could not prepare sync-failure transaction'
REMAGIC_TEST_TRANSACTION_PREPARED=true
remagic_acceptance_set_state finished || fail 'could not mark sync-failure transaction finished'
REMAGIC_FIXTURE_FAIL_COMMAND=sync
export REMAGIC_FIXTURE_FAIL_COMMAND
if remagic_acceptance_remove_test_root >/dev/null 2>&1; then
    fail 'test-root deletion accepted a failed durability barrier'
fi
unset REMAGIC_FIXTURE_FAIL_COMMAND
[ -e "$REMAGIC_TEST_LOCK" ] || fail 'failed deletion barrier released the recovery lock'
[ ! -e "$REMAGIC_TEST_ROOT" ] || fail 'sync-failure fixture did not reach the no-journal phase'
remagic_acceptance_recover_orphan "$$" || fail 'sync-failure transaction was not recoverable'
REMAGIC_TEST_LOCKED=false

PATH=$ORIGINAL_PATH
export PATH

# Run this complete fixture under BusyBox ash when it is available. The guard
# prevents recursion; desktop environments without BusyBox still run all
# semantic and fault-injection assertions under /bin/sh.
if [ "${REMAGIC_FIXTURE_UNDER_BUSYBOX:-0}" != 1 ] && command -v busybox >/dev/null 2>&1; then
    REMAGIC_FIXTURE_UNDER_BUSYBOX=1 busybox ash "$0"
fi

# A process can die after publishing a complete lock but before its atomic
# journal rename. That state is provably mutation-free and must be reclaimable.
mkdir -p "$REMAGIC_TEST_LOCK"
dead_owner=2147483647
printf '%s\n' "$dead_owner" >"$REMAGIC_TEST_LOCK/pid"
printf '%s\n' "$REMAGIC_ACCEPTANCE_FORMAT" >"$REMAGIC_TEST_LOCK/format"
printf '%s\n' "$REMAGIC_TEST_ROOT" >"$REMAGIC_TEST_LOCK/test-root"
printf '%s\n' claimed >"$REMAGIC_TEST_LOCK/state"
prepare_stage=$(remagic_acceptance_prepare_stage_path "$dead_owner")
mkdir -p "$prepare_stage/transaction"
printf '%s\n' "$REMAGIC_ACCEPTANCE_FORMAT" >"$prepare_stage/transaction/format"
remagic_test_lock || fail 'dead claimed lock could not be recovered'
[ ! -e "$prepare_stage" ] || fail 'dead journal staging directory leaked'
remagic_test_release_lock

echo 'device test isolation fixture passed'
