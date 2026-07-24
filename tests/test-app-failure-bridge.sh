#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d /tmp/remagic-app-failure-test.XXXXXX)
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
mkdir -p "$TMP/runtime/magicpaper"
printf '%s\n' '{"generation":42}' >"$TMP/runtime/magicpaper/lifecycle-status.json"

cat >"$TMP/ctl" <<'EOF'
#!/bin/sh
printf '%s\n' "$*" >"$REMAGIC_TEST_CALL"
exit "${REMAGIC_TEST_CTL_STATUS:-0}"
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
REMAGIC_TEST_RECOVERED=$TMP/recovered \
    "$ROOT/scripts/remagic-app-failed" magicpaper
grep -Fxq 'runtime-exited magicpaper --generation 42 --exit-code 1 --crashed' \
    "$TMP/call"
[ ! -e "$TMP/recovered" ]

REMAGIC_CTL=$TMP/ctl \
REMAGIC_RECOVER=$TMP/recover \
REMAGIC_RUNTIME_ROOT=$TMP/runtime \
REMAGIC_TEST_CALL=$TMP/call \
REMAGIC_TEST_CTL_STATUS=1 \
REMAGIC_TEST_RECOVERED=$TMP/recovered \
    "$ROOT/scripts/remagic-app-failed" magicpaper
[ -e "$TMP/recovered" ]

grep -Fxq 'OnFailure=remagic-app-failed@%i.service' \
    "$ROOT/systemd/remagic-app@.service"
! grep -Fq 'OnFailure=remagic-recover.service' \
    "$ROOT/systemd/remagic-app@.service"

echo "application failure bridge fixture passed"
