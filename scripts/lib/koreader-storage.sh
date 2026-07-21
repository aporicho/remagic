#!/bin/sh

# KOReader storage-boundary helpers used by the transactional installer. They
# are separate from generic deployment primitives because they encode the
# adapter's exact data-migration policy.

# Validate every existing directory component without resolving through a
# symlink. Missing trailing components are allowed so callers can use this
# before mkdir -p, then call it again after creation.
deployment_assert_safe_directory_path() {
    directory_path=$1
    canonical_absolute_path "$directory_path" || return 1
    remaining_path=${directory_path#/}
    current_path=
    while [ -n "$remaining_path" ]; do
        case "$remaining_path" in
            */*) path_component=${remaining_path%%/*}; remaining_path=${remaining_path#*/} ;;
            *) path_component=$remaining_path; remaining_path= ;;
        esac
        current_path=$current_path/$path_component
        [ ! -L "$current_path" ] || return 1
        if [ -e "$current_path" ] && [ ! -d "$current_path" ]; then
            return 1
        fi
    done
}

# Managed KOReader data and backup trees contain only real directories and
# regular files. In particular, a dormant symlink elsewhere in the tree must
# not become a write target during a later recursive migration.
deployment_assert_safe_storage_tree() {
    storage_path=$1
    deployment_assert_safe_directory_path "$storage_path" || return 1
    [ -e "$storage_path" ] || [ -L "$storage_path" ] || return 0
    [ -d "$storage_path" ] && [ ! -L "$storage_path" ] || return 1
    unsafe_storage=$(find "$storage_path" ! -type f ! -type d -print -quit 2>/dev/null) || return 1
    [ -z "$unsafe_storage" ]
}

# Return the conservative amount of migratable data across the same colon-
# separated source set passed to the migrator. Real source directories are
# de-duplicated by filesystem identity; roots with a symlink in their path are
# ignored because the migration boundary never follows them.
deployment_koreader_legacy_kb() {
    legacy_roots=$1
    migratable_paths=$2
    legacy_total_kb=0
    seen_legacy_roots='|'
    remaining_roots=$legacy_roots
    while :; do
        case "$remaining_roots" in
            *:*) legacy_root=${remaining_roots%%:*}; remaining_roots=${remaining_roots#*:} ;;
            *) legacy_root=$remaining_roots; remaining_roots= ;;
        esac
        if [ -n "$legacy_root" ] && \
           deployment_assert_safe_directory_path "$legacy_root" 2>/dev/null && \
           [ -d "$legacy_root" ] && [ ! -L "$legacy_root" ]; then
            legacy_identity=$(stat -c '%d:%i' "$legacy_root" 2>/dev/null || true)
            case "$legacy_identity:$seen_legacy_roots" in
                :*|*"|$legacy_identity|"*) ;;
                *)
                    seen_legacy_roots=$seen_legacy_roots$legacy_identity'|'
                    for legacy_name in $migratable_paths; do
                        legacy_path=$legacy_root/$legacy_name
                        [ -e "$legacy_path" ] || [ -L "$legacy_path" ] || continue
                        [ ! -L "$legacy_path" ] || continue
                        deployment_assert_safe_directory_path "${legacy_path%/*}" 2>/dev/null || continue
                        legacy_size=$(du -sk "$legacy_path" 2>/dev/null | awk '{print $1}')
                        legacy_size=${legacy_size:-0}
                        legacy_total_kb=$((legacy_total_kb + legacy_size))
                    done
                    ;;
            esac
        fi
        [ -n "$remaining_roots" ] || break
    done
    printf '%s\n' "$legacy_total_kb"
}
