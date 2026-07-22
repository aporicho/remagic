#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SCRIPT=scripts/device-lock-acceptance-v2.sh

# Acceptance tools belong to the ReMagic system package, not to either user
# application. Keep the source-to-release relationship explicit so the test
# cannot silently fall back to the retired monolithic bundle installer.
grep -Fq 'device-stress-acceptance-v2.sh device-lock-acceptance-v2.sh' \
    "$ROOT/scripts/build-system-release.sh" || {
    echo "lock acceptance script is not staged by the system release builder" >&2
    exit 1
}
grep -Fq '"$PAYLOAD/share/testing/$test_script"' \
    "$ROOT/scripts/build-system-release.sh" || {
    echo "lock acceptance script has no installed testing destination" >&2
    exit 1
}
grep -Fq '/home/root/apps/remagic/share/testing/device-lock-acceptance-v2.sh' \
    "$ROOT/README.md"
grep -Fq 'wait_domain manager 6000' "$ROOT/$SCRIPT"
grep -Fq 'wait_submission_reason "$baseline" unlock_screen 6000' "$ROOT/$SCRIPT"
grep -Fq 'assert_no_submission_reason "$baseline" lock_refresh' "$ROOT/$SCRIPT"
grep -Fq 'panel_failure_count)" = "$baseline_failures"' "$ROOT/$SCRIPT"

echo "lock acceptance inventory tests passed"
