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

cat >"$runtime/magicpaper" <<'EOF'
#!/bin/sh
if [ "${MAGICPAPER_OPENAI_KEY+x}" = x ] || [ "${MAGICPAPER_OCR_TOKEN+x}" = x ]; then
    printf 'credentials-present\n' >>"$TEST_TRACE"
else
    printf 'credentials-absent\n' >>"$TEST_TRACE"
fi
EOF
chmod 0755 "$runtime/magicpaper"
cat >"$legacy/oracle.env" <<'EOF'
MAGICPAPER_OPENAI_KEY=production-sentinel-that-must-not-enter-tests
MAGICPAPER_OCR_TOKEN=production-ocr-sentinel-that-must-not-enter-tests
EOF

for wrapper in magicpaper-qtfb magicpaper-remagic; do
    : >"$trace"
    MAGICPAPER_OPENAI_KEY=inherited-production-sentinel \
    MAGICPAPER_OCR_TOKEN=inherited-ocr-sentinel \
    MAGICPAPER_TEST_MODE=1 \
    MAGICPAPER_CONFIG=$missing_config \
    MAGICPAPER_RUNTIME_ROOT=$runtime \
    MAGICPAPER_LEGACY_ROOT=$legacy \
    MAGICPAPER_ENV_LOADER=$ROOT/scripts/magicpaper-env \
    TEST_TRACE=$trace \
        sh "$ROOT/scripts/$wrapper"
    [ "$(cat "$trace")" = credentials-absent ] || {
        echo "FAIL: $wrapper imported production credentials in test mode" >&2
        exit 1
    }
done

# Preserve the documented upgrade behavior outside deterministic test mode.
: >"$trace"
MAGICPAPER_TEST_MODE=0 \
MAGICPAPER_CONFIG=$missing_config \
MAGICPAPER_RUNTIME_ROOT=$runtime \
MAGICPAPER_LEGACY_ROOT=$legacy \
MAGICPAPER_ENV_LOADER=$ROOT/scripts/magicpaper-env \
TEST_TRACE=$trace \
    sh "$ROOT/scripts/magicpaper-qtfb"
[ "$(cat "$trace")" = credentials-present ] || {
    echo "FAIL: production legacy configuration fallback no longer works" >&2
    exit 1
}

echo "MagicPaper config isolation tests passed"
