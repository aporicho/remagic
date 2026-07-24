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

# systemd considers remagicd active before its asynchronous control socket is
# ready to accept requests.  A system update must wait for the public control
# plane instead of treating that short startup window as an install failure.
wait_for_remagic_ready() {
    ctl=$1
    max_attempts=${2:-20}
    delay=${3:-1}
    attempt=0

    while [ "$attempt" -lt "$max_attempts" ]; do
        if systemctl is-active --quiet remagicd.service && \
            "$ctl" status >/dev/null 2>&1; then
            return 0
        fi
        attempt=$((attempt + 1))
        if [ "$attempt" -lt "$max_attempts" ]; then
            sleep "$delay"
        fi
    done

    echo "ReMagic: manager control socket did not become ready after $max_attempts attempts" >&2
    return 1
}

# A running manager may own the panel through its display host and foreground
# application.  Updating binaries underneath that ownership graph makes the
# daemon's SIGINT handoff race systemd stopping its dependants.  Ask the public
# control plane to complete the stock handoff first and prove the resulting
# domain before the installer stops any ReMagic unit.
wait_for_remagic_system_domain() {
    ctl=$1
    max_attempts=${2:-120}
    delay=${3:-0.1}
    attempt=0

    while [ "$attempt" -lt "$max_attempts" ]; do
        status=$("$ctl" status 2>/dev/null || true)
        if printf '%s\n' "$status" | \
            grep -Eq '"domain"[[:space:]]*:[[:space:]]*"system"'; then
            return 0
        fi
        attempt=$((attempt + 1))
        if [ "$attempt" -lt "$max_attempts" ]; then
            sleep "$delay"
        fi
    done

    echo "ReMagic: stock handoff did not complete before system update" >&2
    return 1
}

prepare_remagic_for_update() {
    ctl=$1
    if ! systemctl is-active --quiet remagicd.service; then
        return 0
    fi
    "$ctl" status >/dev/null 2>&1 || {
        echo "ReMagic: running manager control socket is unavailable" >&2
        return 1
    }
    "$ctl" system >/dev/null 2>&1 || {
        echo "ReMagic: could not request stock handoff before system update" >&2
        return 1
    }
    wait_for_remagic_system_domain "$ctl"
}
