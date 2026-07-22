#!/bin/sh
set -eu

REMAGIC_SUPPORTED_OS_SERIES=${REMAGIC_SUPPORTED_OS_SERIES:-3.27}

rooted_path() {
    root=${REMAGIC_DETECT_ROOT:-/}
    case "$root" in
        /) printf '/%s\n' "${1#/}" ;;
        *) printf '%s/%s\n' "${root%/}" "${1#/}" ;;
    esac
}

read_identity() {
    path=$1
    [ -r "$path" ] || return 1
    tr -d '\000\r\n' <"$path"
}

os_release_field() {
    key=$1
    file=$(rooted_path /etc/os-release)
    [ -r "$file" ] || return 1
    sed -n "s/^${key}=//p" "$file" | sed -n '1{s/^"//;s/"$//;p;}'
}

detect_supported_device() {
    machine=$(read_identity "$(rooted_path /sys/devices/soc0/machine)") || {
        echo "ReMagic: cannot read device machine identity" >&2
        return 1
    }
    model=$(read_identity "$(rooted_path /proc/device-tree/model)") || {
        echo "ReMagic: cannot read device model identity" >&2
        return 1
    }
    [ "$machine" = "$model" ] || {
        echo "ReMagic: machine/model identity mismatch" >&2
        return 1
    }
    case "$machine" in
        'reMarkable Ferrari')
            REMAGIC_DEVICE_PRODUCT=paper_pro
            REMAGIC_DEVICE_CODENAME=ferrari
            ;;
        'reMarkable Chiappa')
            REMAGIC_DEVICE_PRODUCT=paper_pro_move
            REMAGIC_DEVICE_CODENAME=chiappa
            ;;
        *)
            echo "ReMagic: unsupported device: $machine" >&2
            return 1
            ;;
    esac
    REMAGIC_OS_VERSION=$(os_release_field IMG_VERSION) || true
    case "$REMAGIC_OS_VERSION" in
        "$REMAGIC_SUPPORTED_OS_SERIES"|"$REMAGIC_SUPPORTED_OS_SERIES".*) ;;
        '')
            echo "ReMagic: /etc/os-release has no IMG_VERSION" >&2
            return 1
            ;;
        *)
            echo "ReMagic: software $REMAGIC_OS_VERSION is unsupported; expected $REMAGIC_SUPPORTED_OS_SERIES.x" >&2
            return 1
            ;;
    esac
    export REMAGIC_DEVICE_PRODUCT REMAGIC_DEVICE_CODENAME REMAGIC_OS_VERSION
}

release_value() {
    key=$1
    file=$2
    sed -n "s/^${key}=//p" "$file" | sed -n '1p'
}

require_safe_release_value() {
    key=$1
    file=$2
    value=$(release_value "$key" "$file")
    case "$value" in
        ''|*[!A-Za-z0-9._,+/-]*)
            echo "ReMagic: invalid release field $key" >&2
            return 1
            ;;
    esac
    printf '%s\n' "$value"
}
