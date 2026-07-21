#!/bin/sh

# One content identity is shared by the bundle builder and the device-side
# installer.  The KOReader adapter validates its source before staging it; the
# manager deliberately validates that output again before giving it the stable
# MagicPaper filename.
MAGICPAPER_UI_FONT_SHA256=dbbdf59d7035d980abecf4f820e615b72107865a00f6eb41a1bbb9d9d1492fd1
readonly MAGICPAPER_UI_FONT_SHA256

magicpaper_verify_font_sha256() {
    local expected path label actual
    [ "$#" -eq 3 ] || return 2
    expected=$1
    path=$2
    label=$3
    [ "${#expected}" -eq 64 ] || {
        echo "$label has an invalid expected SHA-256" >&2
        return 1
    }
    case $expected in *[!0-9a-f]*)
        echo "$label has an invalid expected SHA-256" >&2
        return 1
        ;;
    esac
    [ -f "$path" ] && [ ! -L "$path" ] && [ -s "$path" ] || {
        echo "$label is missing, empty, or not a regular file: $path" >&2
        return 1
    }
    actual=$(sha256sum "$path" | awk '{print $1}') || return 1
    [ "$actual" = "$expected" ] || {
        echo "$label checksum mismatch: $path" >&2
        return 1
    }
}
