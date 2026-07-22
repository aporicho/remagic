#!/bin/sh

# Versioned KOReader release helpers shared by bundle validation and the
# transactional device installer.  Callers deliberately own cleanup policy.

koreader_release_load() {
    record=$1
    [ -f "$record" ] && [ ! -L "$record" ] || return 1
    KOREADER_VENDOR_RELEASE=$(sed -n 's/^vendor_release=//p' "$record")
    KOREADER_ADAPTER_RELEASE=$(sed -n 's/^adapter_release=//p' "$record")
    [ "$(grep -c '^vendor_release=' "$record")" -eq 1 ] && \
        [ "$(grep -c '^adapter_release=' "$record")" -eq 1 ] || return 1
    case "$KOREADER_VENDOR_RELEASE" in
        v2026.03-[0-9a-f][0-9a-f]*) ;;
        *) return 1 ;;
    esac
    case "$KOREADER_ADAPTER_RELEASE" in
        adapter-[0-9a-f][0-9a-f]*) ;;
        *) return 1 ;;
    esac
    vendor_hash=${KOREADER_VENDOR_RELEASE#v2026.03-}
    adapter_hash=${KOREADER_ADAPTER_RELEASE#adapter-}
    [ "${#vendor_hash}" -eq 64 ] && [ "${#adapter_hash}" -eq 64 ] || return 1
    case "$vendor_hash$adapter_hash" in *[!0-9a-f]*) return 1 ;; esac
    export KOREADER_VENDOR_RELEASE KOREADER_ADAPTER_RELEASE
}

koreader_release_vendor_root() {
    printf '%s/vendor/releases/%s/koreader\n' "$1" "$KOREADER_VENDOR_RELEASE"
}

koreader_release_adapter_root() {
    printf '%s/adapter/releases/%s\n' "$1" "$KOREADER_ADAPTER_RELEASE"
}

koreader_release_adapter_digest() {
    release_root=$1
    (
        cd "$release_root" || exit 1
        raw_paths=$(mktemp /tmp/koreader-for-remagic-adapter-paths.XXXXXX) || exit 1
        sorted_paths=$(mktemp /tmp/koreader-for-remagic-adapter-sorted.XXXXXX) || {
            rm -f "$raw_paths"
            exit 1
        }
        digest_input=$(mktemp /tmp/koreader-for-remagic-adapter-digest.XXXXXX) || {
            rm -f "$raw_paths" "$sorted_paths"
            exit 1
        }
        trap 'rm -f "$raw_paths" "$sorted_paths" "$digest_input"' 0 HUP INT TERM
        find . -print >"$raw_paths" || exit 1
        LC_ALL=C sort "$raw_paths" >"$sorted_paths" || exit 1
        while IFS= read -r path; do
            mode=$(stat -c %a "$path") || exit 1
            if [ -d "$path" ] && [ ! -L "$path" ]; then
                printf 'd\t%s\t%s\n' "$mode" "$path"
            elif [ -f "$path" ] && [ ! -L "$path" ]; then
                hash_line=$(sha256sum "$path") || exit 1
                hash=${hash_line%% *}
                [ "${#hash}" -eq 64 ] || exit 1
                printf 'f\t%s\t%s\t%s\n' "$mode" "$hash" "$path"
            else
                exit 1
            fi
        done <"$sorted_paths" >"$digest_input" || exit 1
        digest_line=$(sha256sum "$digest_input") || exit 1
        digest=${digest_line%% *}
        [ "${#digest}" -eq 64 ] || exit 1
        printf '%s\n' "$digest"
    )
}

koreader_release_verify() (
    application_root=$1
    vendor_root=$(koreader_release_vendor_root "$application_root") || return 1
    adapter_root=$(koreader_release_adapter_root "$application_root") || return 1
    file_manifest=$application_root/deployment/vendor.files
    hash_manifest=$application_root/deployment/vendor.sha256

    [ -d "$vendor_root" ] && [ ! -L "$vendor_root" ] && \
        [ -d "$adapter_root" ] && [ ! -L "$adapter_root" ] && \
        [ -f "$file_manifest" ] && [ ! -L "$file_manifest" ] && \
        [ -f "$hash_manifest" ] && [ ! -L "$hash_manifest" ] || exit 1

    unsafe_paths=$(mktemp /tmp/koreader-for-remagic-unsafe.XXXXXX) || exit 1
    raw_paths=$(mktemp /tmp/koreader-for-remagic-files-raw.XXXXXX) || exit 1
    sorted_paths=$(mktemp /tmp/koreader-for-remagic-files-sorted.XXXXXX) || exit 1
    actual_files=$(mktemp /tmp/koreader-for-remagic-files.XXXXXX) || exit 1
    trap 'rm -f "$unsafe_paths" "$raw_paths" "$sorted_paths" "$actual_files"' \
        0 HUP INT TERM

    find "$vendor_root" ! -type f ! -type d -print >"$unsafe_paths" || exit 1
    find "$adapter_root" ! -type f ! -type d -print >>"$unsafe_paths" || exit 1
    [ ! -s "$unsafe_paths" ] || exit 1

    cd "$application_root" || exit 1
    find "vendor/releases/$KOREADER_VENDOR_RELEASE/koreader" -print \
        >"$raw_paths" || exit 1
    LC_ALL=C sort "$raw_paths" >"$sorted_paths" || exit 1
    while IFS= read -r path; do
        mode=$(stat -c %a "$path") || exit 1
        if [ -d "$path" ] && [ ! -L "$path" ]; then
            entry_type=d
        elif [ -f "$path" ] && [ ! -L "$path" ]; then
            entry_type=f
        else
            exit 1
        fi
        printf '%s\t%s\t%s\n' "$entry_type" "$mode" "$path"
    done <"$sorted_paths" >"$actual_files" || exit 1
    cmp -s "$file_manifest" "$actual_files" || exit 1
    sha256sum -c deployment/vendor.sha256 >/dev/null || exit 1

    actual_adapter_hash=$(koreader_release_adapter_digest "$adapter_root") || exit 1
    [ "adapter-$actual_adapter_hash" = "$KOREADER_ADAPTER_RELEASE" ]
)

koreader_runtime_prepare_writable_paths() {
    config_root=$1
    cache_root=$2
    deployment_lock=$3
    for runtime_root in "$config_root" "$cache_root"; do
        deployment_assert_safe_directory_path "$runtime_root" || return 1
        if [ ! -e "$runtime_root" ] && [ ! -L "$runtime_root" ]; then
            mkdir -p "$runtime_root" || return 1
            chmod 0700 "$runtime_root" || return 1
            chown 0:0 "$runtime_root" || return 1
        fi
        [ -d "$runtime_root" ] && [ ! -L "$runtime_root" ] || return 1
    done
    if [ -e "$deployment_lock" ] || [ -L "$deployment_lock" ]; then
        [ -f "$deployment_lock" ] && [ ! -L "$deployment_lock" ] || return 1
    else
        : >"$deployment_lock" || return 1
    fi
    chmod 0600 "$deployment_lock" && chown 0:0 "$deployment_lock"
}
