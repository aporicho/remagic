#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d /tmp/koreader-for-remagic-release-test.XXXXXX)
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
# shellcheck source=scripts/lib/koreader-release.sh
. "$ROOT/scripts/lib/koreader-release.sh"
MODULES='remagic-lifecycle-protocol.lua
remagic-open-path.lua'
PATCHES='10-remagic-environment.lua
20-remagic-policy.lua
21-remagic-lifecycle-v2.lua'

required_files_block=$(sed -n "/^required_files='/,/^testing\/manifests\/koreader.toml'/p" \
    "$ROOT/scripts/install-device.sh")
validate_bundle_block=$(sed -n '/^validate_bundle() {/,/^preflight_space() {/p' \
    "$ROOT/scripts/install-device.sh")

printf '%s\n' "$MODULES" | while IFS= read -r module; do
    [ "$(printf '%s\n' "$required_files_block" | grep -c "^scripts/$module$")" -eq 1 ] || {
        echo "KOReader module is not covered exactly once by the bundle inventory: $module" >&2
        exit 1
    }
    grep -Fq "KOREADER_ADAPTER/scripts/\$module" "$ROOT/scripts/build-bundle.sh" || {
        echo "KOReader module is not sourced from the adapter build: $module" >&2
        exit 1
    }
    grep -Fq '"$KOREADER_ADAPTER_STAGE/libexec/$module"' \
        "$ROOT/scripts/build-bundle.sh" || {
        echo "KOReader module is not staged into the content-addressed adapter: $module" >&2
        exit 1
    }
    printf '%s\n' "$validate_bundle_block" | grep -Fq \
        "libexec/$module" || {
        echo "KOReader release module is absent from preflight inventory: $module" >&2
        exit 1
    }
done

printf '%s\n' "$PATCHES" | while IFS= read -r patch; do
    grep -Fq "$patch" "$ROOT/scripts/build-bundle.sh"
    printf '%s\n' "$validate_bundle_block" | grep -Fq "share/patches/$patch"
done

grep -Fq 'koreader_release_adapter_digest "$KOREADER_ADAPTER_STAGE"' \
    "$ROOT/scripts/build-bundle.sh"
grep -Fq 'koreader_release_verify "$SOURCE_DIR/opt/koreader-for-remagic"' \
    "$ROOT/scripts/install-device.sh"
! grep -Rq 'remagic-lifecycle-async.lua\|scripts/koreader-lifecycle' \
    "$ROOT/scripts" "$ROOT/tests/test-koreader-adapter-inventory.sh"

vendor_hash=56621d5ee66ad94f4f3e2e6d204e8c34be730343f915edc36bb076a043a2e468
vendor_release=v2026.03-$vendor_hash
mkdir -p "$TMP/vendor/releases/$vendor_release/koreader" \
    "$TMP/adapter/releases/.stage/bin" "$TMP/deployment"
printf 'official\n' >"$TMP/vendor/releases/$vendor_release/koreader/reader.lua"
printf 'adapter\n' >"$TMP/adapter/releases/.stage/bin/koreader-for-remagic"
adapter_hash=$(koreader_release_adapter_digest "$TMP/adapter/releases/.stage")
adapter_release=adapter-$adapter_hash
mv "$TMP/adapter/releases/.stage" "$TMP/adapter/releases/$adapter_release"
printf 'vendor_release=%s\nadapter_release=%s\n' \
    "$vendor_release" "$adapter_release" >"$TMP/deployment/current.env"
(
    cd "$TMP"
    find "vendor/releases/$vendor_release/koreader" -print \
        >deployment/vendor.files.unsorted
    LC_ALL=C sort deployment/vendor.files.unsorted >deployment/vendor.paths
    while IFS= read -r path; do
        mode=$(stat -c %a "$path")
        if [ -d "$path" ]; then entry_type=d; else entry_type=f; fi
        printf '%s\t%s\t%s\n' "$entry_type" "$mode" "$path"
    done <deployment/vendor.paths >deployment/vendor.files
    rm -f deployment/vendor.files.unsorted deployment/vendor.paths
    sha256sum "vendor/releases/$vendor_release/koreader/reader.lua" \
        >deployment/vendor.sha256
)
koreader_release_load "$TMP/deployment/current.env"
koreader_release_verify "$TMP"
printf 'tampered\n' >>"$TMP/vendor/releases/$vendor_release/koreader/reader.lua"
if koreader_release_verify "$TMP" >/dev/null 2>&1; then
    echo "KOReader vendor mutation escaped release verification" >&2
    exit 1
fi
printf 'official\n' >"$TMP/vendor/releases/$vendor_release/koreader/reader.lua"
chmod 0600 "$TMP/vendor/releases/$vendor_release/koreader/reader.lua"
if koreader_release_verify "$TMP" >/dev/null 2>&1; then
    echo "KOReader vendor mode mutation escaped release verification" >&2
    exit 1
fi
chmod 0644 "$TMP/vendor/releases/$vendor_release/koreader/reader.lua"
ln -s /etc/passwd "$TMP/vendor/releases/$vendor_release/koreader/unsafe-link"
if koreader_release_verify "$TMP" >/dev/null 2>&1; then
    echo "KOReader vendor symlink escaped release verification" >&2
    exit 1
fi
rm -f "$TMP/vendor/releases/$vendor_release/koreader/unsafe-link"
chmod 0777 "$TMP/vendor/releases/$vendor_release/koreader"
if koreader_release_verify "$TMP" >/dev/null 2>&1; then
    echo "KOReader vendor directory mode mutation escaped release verification" >&2
    exit 1
fi
chmod 0755 "$TMP/vendor/releases/$vendor_release/koreader"
mkdir "$TMP/vendor/releases/$vendor_release/koreader/untracked-empty"
if koreader_release_verify "$TMP" >/dev/null 2>&1; then
    echo "KOReader vendor empty-directory injection escaped release verification" >&2
    exit 1
fi
rmdir "$TMP/vendor/releases/$vendor_release/koreader/untracked-empty"
mkdir "$TMP/adapter/releases/$adapter_release/untracked-empty"
if koreader_release_verify "$TMP" >/dev/null 2>&1; then
    echo "KOReader adapter empty-directory injection escaped release verification" >&2
    exit 1
fi
rmdir "$TMP/adapter/releases/$adapter_release/untracked-empty"
chmod 0777 "$TMP/adapter/releases/$adapter_release/bin/koreader-for-remagic"
if koreader_release_verify "$TMP" >/dev/null 2>&1; then
    echo "KOReader adapter mode mutation escaped release verification" >&2
    exit 1
fi

echo "KOReader adapter inventory tests passed"
