#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
MIGRATOR=$ROOT/scripts/magicpaper-data-migrate
TMP=$(mktemp -d /tmp/magicpaper-data-migrate-test.XXXXXX)
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

run_migrator() {
    MAGICPAPER_DATA_ROOT=$TMP/new/data \
    MAGICPAPER_CONFIG_ROOT=$TMP/new/config \
    MAGICPAPER_LEGACY_DATA_ROOTS=$TMP/intermediate/data:$TMP/legacy/data \
    MAGICPAPER_LEGACY_CONFIG_ROOTS=$TMP/intermediate/config:$TMP/legacy/config \
    MAGICPAPER_LEGACY_CONFIG_FILE=$TMP/legacy-app/oracle.env \
        "$MIGRATOR"
}

mkdir -p "$TMP/legacy/data/tasks" "$TMP/legacy/config"
printf 'legacy task\n' >"$TMP/legacy/data/tasks/tasks.json"
printf 'RIDDLE_OPENAI_KEY=legacy-secret\n' >"$TMP/legacy/config/oracle.env"
run_migrator

grep -qx 'legacy task' "$TMP/new/data/tasks/tasks.json"
grep -qx 'RIDDLE_OPENAI_KEY=legacy-secret' "$TMP/new/config/oracle.env"
grep -qx 'legacy task' "$TMP/legacy/data/tasks/tasks.json"
[ "$(stat -c %a "$TMP/new/data")" = 700 ]
[ "$(stat -c %a "$TMP/new/config")" = 700 ]

# New storage always wins and is never overwritten by a subsequent migration.
printf 'current task\n' >"$TMP/new/data/tasks/tasks.json"
printf 'MAGICPAPER_OPENAI_KEY=current-secret\n' >"$TMP/new/config/oracle.env"
run_migrator
grep -qx 'current task' "$TMP/new/data/tasks/tasks.json"
grep -qx 'MAGICPAPER_OPENAI_KEY=current-secret' "$TMP/new/config/oracle.env"

# The intermediate public-name layout takes precedence over the oldest riddle
# layout on a device that has not yet created the final tree.
rm -rf "$TMP/new"
mkdir -p "$TMP/intermediate/data/todos" "$TMP/intermediate/config"
printf 'intermediate todo\n' >"$TMP/intermediate/data/todos/todos.json"
printf 'MAGICPAPER_OPENAI_KEY=intermediate-secret\n' >"$TMP/intermediate/config/oracle.env"
run_migrator
grep -qx 'intermediate todo' "$TMP/new/data/todos/todos.json"
grep -qx 'MAGICPAPER_OPENAI_KEY=intermediate-secret' "$TMP/new/config/oracle.env"

# A symlink in a managed root is rejected instead of copying credentials or
# persistent data through an attacker-controlled path.
rm -rf "$TMP/new"
mkdir -p "$TMP/elsewhere"
ln -s "$TMP/elsewhere" "$TMP/new"
if run_migrator >/dev/null 2>&1; then
    echo "MagicPaper migrator accepted a symlinked target" >&2
    exit 1
fi

# Legacy trees containing symlinks or special objects are rejected rather
# than importing paths outside the owned data root.
rm -rf "$TMP/new"
mkdir -p "$TMP/intermediate/data"
ln -s "$TMP/elsewhere" "$TMP/intermediate/data/unsafe-link"
if run_migrator >/dev/null 2>&1; then
    echo "MagicPaper migrator accepted a symlinked legacy object" >&2
    exit 1
fi

echo "MagicPaper data migration tests passed"
