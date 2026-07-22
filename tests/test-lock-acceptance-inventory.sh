#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SCRIPT=scripts/device-lock-acceptance-v2.sh
required_files_block=$(sed -n "/^required_files='/,/^testing\/manifests\/koreader.toml'/p" \
    "$ROOT/scripts/install-device.sh")
stage_manager_block=$(sed -n '/^stage_manager() {/,/^stage_adapter() {/p' \
    "$ROOT/scripts/install-device.sh")

[ "$(printf '%s\n' "$required_files_block" | grep -c "^$SCRIPT$")" -eq 1 ] || {
    echo "lock acceptance script is not covered exactly once by the bundle inventory" >&2
    exit 1
}
printf '%s\n' "$stage_manager_block" | grep -Fq 'device-lock-acceptance-v2.sh' || {
    echo "lock acceptance script is not staged into the installed manager" >&2
    exit 1
}
grep -Fq '/home/root/apps/remagic/share/device-lock-acceptance-v2.sh' "$ROOT/README.md"
grep -Fq 'wait_domain manager 6000' "$ROOT/$SCRIPT"
grep -Fq 'wait_submission_reason "$baseline" unlock_screen 6000' "$ROOT/$SCRIPT"
grep -Fq 'assert_no_submission_reason "$baseline" lock_refresh' "$ROOT/$SCRIPT"
grep -Fq 'panel_failure_count)" = "$baseline_failures"' "$ROOT/$SCRIPT"

echo "lock acceptance inventory tests passed"
