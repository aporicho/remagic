#!/bin/sh
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
TMPDIR_TEST=$(mktemp -d)
trap 'rm -rf "$TMPDIR_TEST"' EXIT HUP INT TERM

runtime=$TMPDIR_TEST/runtime
legacy=$TMPDIR_TEST/legacy
missing_config=$TMPDIR_TEST/missing/oracle.env
trace=$TMPDIR_TEST/trace
mkdir -p "$runtime" "$legacy"

cat >"$runtime/riddle-qtfb" <<'EOF'
#!/bin/sh
if [ "${RIDDLE_OPENAI_KEY+x}" = x ] || [ "${RIDDLE_OCR_TOKEN+x}" = x ]; then
    printf 'credentials-present\n' >>"$TEST_TRACE"
else
    printf 'credentials-absent\n' >>"$TEST_TRACE"
fi
EOF
chmod 0755 "$runtime/riddle-qtfb"
cat >"$legacy/oracle.env" <<'EOF'
RIDDLE_OPENAI_KEY=production-sentinel-that-must-not-enter-tests
RIDDLE_OCR_TOKEN=production-ocr-sentinel-that-must-not-enter-tests
EOF

for wrapper in magicpaper-qtfb magicpaper-remagic; do
    : >"$trace"
    RIDDLE_OPENAI_KEY=inherited-production-sentinel \
    RIDDLE_OCR_TOKEN=inherited-ocr-sentinel \
    RIDDLE_TEST_MODE=1 \
    RIDDLE_CONFIG=$missing_config \
    MAGICPAPER_RUNTIME_ROOT=$runtime \
    MAGICPAPER_LEGACY_ROOT=$legacy \
    MAGICPAPER_ENV_LOADER=$ROOT/scripts/riddle-env \
    TEST_TRACE=$trace \
        sh "$ROOT/scripts/$wrapper"
    [ "$(cat "$trace")" = credentials-absent ] || {
        echo "FAIL: $wrapper imported production credentials in test mode" >&2
        exit 1
    }
done

# Preserve the documented upgrade behavior outside deterministic test mode.
: >"$trace"
RIDDLE_TEST_MODE=0 \
RIDDLE_CONFIG=$missing_config \
MAGICPAPER_RUNTIME_ROOT=$runtime \
MAGICPAPER_LEGACY_ROOT=$legacy \
MAGICPAPER_ENV_LOADER=$ROOT/scripts/riddle-env \
TEST_TRACE=$trace \
    sh "$ROOT/scripts/magicpaper-qtfb"
[ "$(cat "$trace")" = credentials-present ] || {
    echo "FAIL: production legacy configuration fallback no longer works" >&2
    exit 1
}

echo "MagicPaper config isolation tests passed"
