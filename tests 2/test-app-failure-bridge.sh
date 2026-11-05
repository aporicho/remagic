#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d /tmp/remagic-app-failure-test.XXXXXX)
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
mkdir -p "$TMP/runtime/magicpaper"
printf '%s\n' '{"generation":42}' >"$TMP/runtime/magicpaper/lifecycle-status.json"

cat >"$TMP/ctl" <<'EOF'
#!/bin/sh
case ${1:-} in
    runtime-exited)
        printf '%s\n' "$*" >"$REMAGIC_TEST_CALL"
        exit "${REMAGIC_TEST_REPORT_STATUS:-0}"
        ;;
    status)
        printf '%s\n' "$*" >"$REMAGIC_TEST_HEALTH_CALL"
        [ "${REMAGIC_TEST_HEALTH_STATUS:-0}" -eq 0 ] || exit "$REMAGIC_TEST_HEALTH_STATUS"
        output=${REMAGIC_TEST_HEALTH_OUTPUT:-}
        [ -n "$output" ] || output='{"type":"status","domain":"manager"}'
        printf '%s\n' "$output"
        exit 0
        ;;
    *) exit 2 ;;
esac
EOF
cat >"$TMP/recover" <<'EOF'
#!/bin/sh
: >"$REMAGIC_TEST_RECOVERED"
EOF
chmod 0755 "$TMP/ctl" "$TMP/recover"

REMAGIC_CTL=$TMP/ctl \
REMAGIC_RECOVER=$TMP/recover \
REMAGIC_RUNTIME_ROOT=$TMP/runtime \
REMAGIC_TEST_CALL=$TMP/call \
REMAGIC_TEST_HEALTH_CALL=$TMP/health-call \
REMAGIC_TEST_RECOVERED=$TMP/recovered \
    "$ROOT/scripts/remagic-app-failed" magicpaper
grep -Fxq 'runtime-exited magicpaper --generation 42 --exit-code 1 --crashed' \
    "$TMP/call"
[ ! -e "$TMP/recovered" ]
[ ! -e "$TMP/health-call" ]

# A pre-ready crash has no lifecycle status. The healthy daemon owns its
# synchronous launch rollback, so application OnFailure must not restore the
# entire stock domain.
rm -f "$TMP/runtime/magicpaper/lifecycle-status.json"
REMAGIC_CTL=$TMP/ctl \
REMAGIC_RECOVER=$TMP/recover \
REMAGIC_RUNTIME_ROOT=$TMP/runtime \
REMAGIC_TEST_CALL=$TMP/call \
REMAGIC_TEST_HEALTH_CALL=$TMP/health-call \
REMAGIC_TEST_RECOVERED=$TMP/recovered \
    "$ROOT/scripts/remagic-app-failed" magicpaper
grep -Fxq status "$TMP/health-call"
[ ! -e "$TMP/recovered" ]

# A generation report may lose a race with launch rollback. The daemon still
# owns the application failure if its state observably converges.
printf '%s\n' '{"generation":43}' >"$TMP/runtime/magicpaper/lifecycle-status.json"
REMAGIC_CTL=$TMP/ctl \
REMAGIC_RECOVER=$TMP/recover \
REMAGIC_RUNTIME_ROOT=$TMP/runtime \
REMAGIC_TEST_CALL=$TMP/call \
REMAGIC_TEST_REPORT_STATUS=1 \
REMAGIC_TEST_HEALTH_CALL=$TMP/health-call \
REMAGIC_TEST_RECOVERED=$TMP/recovered \
    "$ROOT/scripts/remagic-app-failed" magicpaper
grep -Fxq 'runtime-exited magicpaper --generation 43 --exit-code 1 --crashed' \
    "$TMP/call"
grep -Fxq status "$TMP/health-call"
[ ! -e "$TMP/recovered" ]

# Liveness is not convergence: a daemon that still reports the dead app as
# foreground must fall back to whole-domain recovery after the bounded wait.
REMAGIC_CTL=$TMP/ctl \
REMAGIC_RECOVER=$TMP/recover \
REMAGIC_RUNTIME_ROOT=$TMP/runtime \
REMAGIC_TEST_CALL=$TMP/call \
REMAGIC_TEST_REPORT_STATUS=1 \
REMAGIC_TEST_HEALTH_CALL=$TMP/health-call \
REMAGIC_TEST_HEALTH_OUTPUT='{"type":"status","domain":{"foreground":"magicpaper"}}' \
REMAGIC_FAILURE_HEALTH_ATTEMPTS=1 \
REMAGIC_TEST_RECOVERED=$TMP/recovered \
    "$ROOT/scripts/remagic-app-failed" magicpaper
[ -e "$TMP/recovered" ]

# Only a persistently unavailable manager control plane also permits the
# emergency whole-domain recovery helper.
rm -f "$TMP/recovered"
REMAGIC_CTL=$TMP/ctl \
REMAGIC_RECOVER=$TMP/recover \
REMAGIC_RUNTIME_ROOT=$TMP/runtime \
REMAGIC_TEST_CALL=$TMP/call \
REMAGIC_TEST_REPORT_STATUS=1 \
REMAGIC_TEST_HEALTH_CALL=$TMP/health-call \
REMAGIC_TEST_HEALTH_STATUS=1 \
REMAGIC_FAILURE_HEALTH_ATTEMPTS=1 \
REMAGIC_TEST_RECOVERED=$TMP/recovered \
    "$ROOT/scripts/remagic-app-failed" magicpaper
[ -e "$TMP/recovered" ]

# A malformed systemd instance cannot claim display ownership; a healthy
# manager remains the only recovery authority.
rm -f "$TMP/recovered"
REMAGIC_CTL=$TMP/ctl \
REMAGIC_RECOVER=$TMP/recover \
REMAGIC_RUNTIME_ROOT=$TMP/runtime \
REMAGIC_TEST_CALL=$TMP/call \
REMAGIC_TEST_HEALTH_CALL=$TMP/health-call \
REMAGIC_TEST_RECOVERED=$TMP/recovered \
    "$ROOT/scripts/remagic-app-failed" 'bad/app'
[ ! -e "$TMP/recovered" ]

grep -Fxq 'OnFailure=remagic-app-failed@%i.service' \
    "$ROOT/systemd/remagic-app@.service"
! grep -Fq 'OnFailure=remagic-recover.service' \
    "$ROOT/systemd/remagic-app@.service"

echo "application failure bridge fixture passed"
