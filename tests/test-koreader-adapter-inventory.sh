#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
MODULES='remagic-lifecycle-async.lua
remagic-lifecycle-protocol.lua
remagic-open-path.lua'

required_files_block=$(sed -n "/^required_files='/,/^testing\/manifests\/koreader.toml'/p" \
    "$ROOT/scripts/install-device.sh")
stage_adapter_block=$(cat "$ROOT/scripts/lib/koreader-adapter-stage.sh")

printf '%s\n' "$MODULES" | while IFS= read -r module; do
    [ "$(printf '%s\n' "$required_files_block" | grep -c "^scripts/$module$")" -eq 1 ] || {
        echo "KOReader module is not covered exactly once by the bundle inventory: $module" >&2
        exit 1
    }
    printf '%s\n' "$stage_adapter_block" | grep -Fq "$module" || {
        echo "KOReader module is not staged into the adapter: $module" >&2
        exit 1
    }
    grep -Fq "KOREADER_ADAPTER/scripts/\$module" "$ROOT/scripts/build-bundle.sh" || {
        echo "KOReader module is not sourced from the adapter build: $module" >&2
        exit 1
    }
    grep -Fq "ADAPTER_ROOT/libexec/\$module" "$ROOT/scripts/install-device.sh" || {
        echo "KOReader live module is not verified after the tree switch: $module" >&2
        exit 1
    }
done

echo "KOReader adapter inventory tests passed"
