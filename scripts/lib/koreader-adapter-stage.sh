#!/bin/sh
# Shared inventory for the small Remagic-specific layer around KOReader.
# The caller supplies `stage_file` so ownership and mode policy remain part of
# the install transaction rather than being duplicated here.

deployment_stage_koreader_adapter_files() {
    stage=$1
    source_dir=$2
    rm -rf "$stage"
    mkdir -p "$stage/bin" "$stage/libexec"
    stage_file 0755 "$source_dir/scripts/koreader-remagic" "$stage/bin/koreader-remagic"
    for helper in koreader-lifecycle koreader-data-migrate koreader-db-inspect \
        koreader-library-sync koreader-not-running; do
        stage_file 0755 "$source_dir/scripts/$helper" "$stage/libexec/$helper"
    done
    stage_file 0644 "$source_dir/scripts/koreader-db-inspect.lua" \
        "$stage/libexec/koreader-db-inspect.lua"
    stage_file 0644 "$source_dir/scripts/koreader-library-index.lua" \
        "$stage/libexec/koreader-library-index.lua"
    for module in remagic-lifecycle-async.lua remagic-lifecycle-protocol.lua \
        remagic-open-path.lua; do
        stage_file 0644 "$source_dir/scripts/$module" "$stage/libexec/$module"
    done
    for patch in 1-remagic-storage.lua 2-remagic-runtime.lua; do
        stage_file 0644 "$source_dir/opt/remagic-koreader/share/patches/$patch" \
            "$stage/share/patches/$patch"
    done
}
