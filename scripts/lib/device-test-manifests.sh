#!/bin/sh

# Materialize isolated acceptance manifests against immutable installed
# package releases. This keeps cold-start migration away from mutable
# `current` symlinks without coupling the system release to application
# content hashes.

remagic_test_resolve_package_path() {
    local app path current target content suffix
    [ "$#" -eq 2 ] || return 1
    app=$1
    path=$2
    case $app in magicpaper|koreader) ;; *) return 1 ;; esac
    case $REMAGIC_TEST_APPS_ROOT$path in *[!A-Za-z0-9_./@+-]*) return 1 ;; esac
    current=$REMAGIC_TEST_APPS_ROOT/$app/current
    case $path in
        "$current") suffix= ;;
        "$current"/*) suffix=/${path#"$current"/} ;;
        "$REMAGIC_TEST_APPS_ROOT/$app/releases/"*)
            printf '%s\n' "$path"
            return 0
            ;;
        *) return 1 ;;
    esac
    target=$(readlink "$current" 2>/dev/null) || return 1
    case $target in releases/*) ;; *) return 1 ;; esac
    content=${target#releases/}
    [ "${#content}" -eq 64 ] || return 1
    case $content in *[!0-9a-f]*) return 1 ;; esac
    printf '%s/%s%s\n' "$REMAGIC_TEST_APPS_ROOT/$app" "$target" "$suffix"
}

remagic_test_materialize_magicpaper() {
    local destination template_root production payload_root
    [ "$#" -eq 2 ] || return 1
    destination=$1
    template_root=$2
    production=$REMAGIC_TEST_PRODUCTION_MANIFEST_ROOT/magicpaper.toml
    [ -s "$production" ] || {
        echo "acceptance isolation: installed MagicPaper manifest is missing" >&2
        return 1
    }
    payload_root=$(sed -n '/^\[/q; s/^working_dir = "\([^"]*\)"$/\1/p' "$production")
    payload_root=$(remagic_test_resolve_package_path magicpaper "$payload_root") || {
        echo "acceptance isolation: installed MagicPaper payload path is unsafe" >&2
        return 1
    }
    case $payload_root in "$REMAGIC_TEST_APPS_ROOT/magicpaper/releases/"*/payload) ;; *)
        echo "acceptance isolation: installed MagicPaper payload layout is invalid" >&2
        return 1
    esac
    sed -e "s|$template_root|$REMAGIC_TEST_ROOT|g" \
        -e "s|/__REMAGIC_MAGICPAPER_PAYLOAD_ROOT__|$payload_root|g" \
        "$REMAGIC_TEST_TEMPLATES/magicpaper.toml" >"$destination" || return 1
    if grep -q '__REMAGIC_' "$destination"; then
        echo "acceptance isolation: unresolved MagicPaper manifest placeholder" >&2
        return 1
    fi
}

remagic_test_materialize_koreader() {
    local destination template_root production exec_path adapter_root vendor_root
    [ "$#" -eq 2 ] || return 1
    destination=$1
    template_root=$2
    production=$REMAGIC_TEST_PRODUCTION_MANIFEST_ROOT/koreader.toml
    [ -s "$production" ] || {
        echo "acceptance isolation: installed KOReader manifest is missing" >&2
        return 1
    }
    exec_path=$(sed -n '/^\[/q; s/^exec = "\([^"]*\)"$/\1/p' "$production")
    vendor_root=$(sed -n '/^\[/q; s/^working_dir = "\([^"]*\)"$/\1/p' "$production")
    exec_path=$(remagic_test_resolve_package_path koreader "$exec_path") || {
        echo "acceptance isolation: installed KOReader executable cannot be resolved" >&2
        return 1
    }
    vendor_root=$(remagic_test_resolve_package_path koreader "$vendor_root") || {
        echo "acceptance isolation: installed KOReader working directory cannot be resolved" >&2
        return 1
    }
    case $exec_path in
        "$REMAGIC_TEST_APPS_ROOT/koreader/releases/"*/payload/*/bin/koreader-for-remagic) ;;
        *) echo "acceptance isolation: installed KOReader executable is unsafe" >&2; return 1 ;;
    esac
    case $vendor_root in
        "$REMAGIC_TEST_APPS_ROOT/koreader/releases/"*/payload/*) ;;
        *) echo "acceptance isolation: installed KOReader working directory is unsafe" >&2; return 1 ;;
    esac
    case $exec_path$vendor_root in *[!A-Za-z0-9_./-]*)
        echo "acceptance isolation: installed KOReader paths contain unsafe characters" >&2
        return 1
    esac
    adapter_root=${exec_path%/bin/koreader-for-remagic}
    sed -e "s|$template_root|$REMAGIC_TEST_ROOT|g" \
        -e "s|/__REMAGIC_KOREADER_ADAPTER_ROOT__|$adapter_root|g" \
        -e "s|/__REMAGIC_KOREADER_VENDOR_ROOT__|$vendor_root|g" \
        "$REMAGIC_TEST_TEMPLATES/koreader.toml" >"$destination" || return 1
    if grep -q '__REMAGIC_' "$destination"; then
        echo "acceptance isolation: unresolved KOReader manifest placeholder" >&2
        return 1
    fi
}

remagic_test_materialize_manifests() {
    local destination template_root
    [ "$#" -eq 1 ] || return 1
    destination=$1
    template_root=/home/root/.local/state/remagic/acceptance/current
    remagic_test_materialize_magicpaper "$destination/magicpaper.toml" "$template_root" || return 1
    remagic_test_materialize_koreader "$destination/koreader.toml" "$template_root" || return 1
    chmod 0644 "$destination/magicpaper.toml" "$destination/koreader.toml"
}
