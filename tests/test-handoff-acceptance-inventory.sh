#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
SCRIPT=scripts/device-handoff-acceptance-v2.sh

[ -x "$ROOT/$SCRIPT" ] || {
    echo "handoff acceptance script is not executable" >&2
    exit 1
}
grep -Fq 'device-handoff-acceptance-v2.sh device-power-audit.sh' \
    "$ROOT/scripts/build-system-release.sh"
grep -Fq '/home/root/apps/remagic/share/testing/device-handoff-acceptance-v2.sh' \
    "$ROOT/README.md"
grep -Fq '. "$ISOLATION"' "$ROOT/$SCRIPT"
grep -Fq "printf 'read\\n'" "$ROOT/$SCRIPT"
grep -Fq 'wait_domain '\''"foreground": "koreader"'\'' 50' "$ROOT/$SCRIPT"
grep -Fq 'event=test-event-consumed command=read' "$ROOT/$SCRIPT"
grep -Fq 'event=reader-handoff-accepted request=[^ ]+ app=koreader' "$ROOT/$SCRIPT"
grep -Fq 'before_full + 1' "$ROOT/$SCRIPT"
grep -Fq 'MAGICPAPER_TEST_EVENT_FILE' "$ROOT/testing/manifests/magicpaper.toml"

echo "handoff acceptance inventory tests passed"
