#!/bin/bash
set -euo pipefail

DEVICE_SOURCE=${BASH_SOURCE[0]}
DEVICE_ROOT=$(CDPATH= cd -- "$(dirname -- "$DEVICE_SOURCE")/../.." && pwd)
USB_PROXY=$DEVICE_ROOT/scripts/lib/usb-tcp-proxy.py
USB_HOST=${REMAGIC_USB_HOST:-10.11.99.1}

usb_usage() {
    cat <<EOF
Usage: $(basename "$0") [probe|ssh|push|pull|install|deploy|power-audit-begin|power-audit-collect] [arguments...]

  probe                 show the matched interface and device identity
  ssh [COMMAND...]      open a shell or run a remote command (default)
  push LOCAL REMOTE     upload one file or directory to the device
  pull REMOTE LOCAL     download one file or directory from the device
  install               download and install the latest stable ReMagic release
  deploy                build and install the current ReMagic checkout
  power-audit-begin      record a plugged-in power baseline before unplugging
  power-audit-collect    collect and validate logs after reconnecting USB
EOF
}

usb_ssh_options() {
    local interface=$1 alias=$2
    USB_SSH_OPTIONS=(
        -F /dev/null
        -o "HostName=$USB_HOST"
        -o "HostKeyAlias=$alias"
        -o "ProxyCommand=$USB_PROXY $interface %h %p"
        -o ConnectTimeout=6
        -o ControlMaster=no
        -o ControlPath=none
        -o StrictHostKeyChecking=accept-new
    )
}

usb_interfaces() {
    ip -o -4 address show | awk -v host="$USB_HOST" '
        $4 ~ /^10\.11\.99\./ {
            split($4, address, "/")
            if (address[1] != host) print $2
        }
    ' | sort -u
}

usb_safe_interface() {
    case "$1" in
        ''|*[!A-Za-z0-9_.:-]*) return 1 ;;
        *) return 0 ;;
    esac
}

usb_probe_machine() {
    local interface=$1
    # USB interface names describe the host port, not the tablet. Either
    # device may appear on a previously used interface after cables are moved,
    # so a persistent host-key alias derived from the interface would reject a
    # legitimate port swap. This probe only discovers the model over a local
    # link; the selected device is authenticated again below with its stable
    # per-model alias before any requested operation is run.
    local probe_options=(
        -F /dev/null
        -o "HostName=$USB_HOST"
        -o "ProxyCommand=$USB_PROXY $interface %h %p"
        -o ConnectTimeout=6
        -o ControlMaster=no
        -o ControlPath=none
        -o UserKnownHostsFile=/dev/null
        -o GlobalKnownHostsFile=/dev/null
        -o StrictHostKeyChecking=no
        -o LogLevel=ERROR
    )
    ssh -n "${probe_options[@]}" -o BatchMode=yes root@remagic-device \
        'tr -d "\000" </sys/devices/soc0/machine' 2>/dev/null
}

usb_select_interface() {
    local interface machine match=""
    if [ -n "${REMAGIC_USB_INTERFACE:-}" ]; then
        interface=$REMAGIC_USB_INTERFACE
        usb_safe_interface "$interface" || {
            echo "$DEVICE_LABEL: invalid interface name: $interface" >&2
            return 1
        }
        [ -d "/sys/class/net/$interface" ] || {
            echo "$DEVICE_LABEL: interface not found: $interface" >&2
            return 1
        }
        machine=$(usb_probe_machine "$interface") || {
            echo "$DEVICE_LABEL: cannot reach the device on $interface" >&2
            return 1
        }
        [ "$machine" = "$DEVICE_MACHINE" ] || {
            echo "$DEVICE_LABEL: $interface is '$machine', expected '$DEVICE_MACHINE'" >&2
            return 1
        }
        printf '%s\n' "$interface"
        return
    fi

    while IFS= read -r interface; do
        [ -n "$interface" ] || continue
        usb_safe_interface "$interface" || continue
        machine=$(usb_probe_machine "$interface" || true)
        if [ "$machine" = "$DEVICE_MACHINE" ]; then
            if [ -n "$match" ]; then
                echo "$DEVICE_LABEL: more than one matching USB device is connected" >&2
                return 1
            fi
            match=$interface
        fi
    done < <(usb_interfaces)

    [ -n "$match" ] || {
        echo "$DEVICE_LABEL: no connected '$DEVICE_MACHINE' device was found" >&2
        return 1
    }
    printf '%s\n' "$match"
}

usb_main() {
    [ -x "$USB_PROXY" ] || {
        echo "$DEVICE_LABEL: missing executable USB proxy: $USB_PROXY" >&2
        return 1
    }

    case "${1:-ssh}" in
        -h|--help|help)
            usb_usage
            return
            ;;
    esac

    local operation=${1:-ssh} interface
    if [ "$#" -gt 0 ]; then shift; fi
    interface=$(usb_select_interface)
    usb_ssh_options "$interface" "$DEVICE_ALIAS"

    case "$operation" in
        probe)
            ssh "${USB_SSH_OPTIONS[@]}" -o BatchMode=yes root@remagic-device \
                'printf "machine="; tr -d "\000" </sys/devices/soc0/machine; printf "\nserial="; cat /sys/devices/soc0/serial_number; printf "os="; . /etc/os-release; printf "%s\n" "${IMG_VERSION:-unknown}"'
            printf 'interface=%s\n' "$interface"
            ;;
        ssh)
            exec ssh "${USB_SSH_OPTIONS[@]}" root@remagic-device "$@"
            ;;
        push)
            [ "$#" -eq 2 ] || { usb_usage >&2; return 2; }
            exec scp "${USB_SSH_OPTIONS[@]}" -O -r -- "$1" "root@remagic-device:$2"
            ;;
        pull)
            [ "$#" -eq 2 ] || { usb_usage >&2; return 2; }
            exec scp "${USB_SSH_OPTIONS[@]}" -O -r -- "root@remagic-device:$1" "$2"
            ;;
        install)
            [ "$#" -eq 0 ] || { usb_usage >&2; return 2; }
            REMAGIC_USB_INTERFACE=$interface \
                REMAGIC_USB_HOST=$USB_HOST \
                REMAGIC_USB_PROXY=$USB_PROXY \
                REMAGIC_USB_ALIAS=$DEVICE_ALIAS \
                "$DEVICE_ROOT/install.sh"
            ;;
        deploy)
            [ "$#" -eq 0 ] || { usb_usage >&2; return 2; }
            REMAGIC_USB_INTERFACE=$interface \
                REMAGIC_USB_HOST=$USB_HOST \
                REMAGIC_USB_ALIAS=$DEVICE_ALIAS \
                "$DEVICE_ROOT/scripts/deploy-usb.sh"
            ;;
        power-audit-begin|power-audit-collect)
            [ "$#" -eq 0 ] || { usb_usage >&2; return 2; }
            audit_operation=${operation#power-audit-}
            exec ssh "${USB_SSH_OPTIONS[@]}" root@remagic-device \
                "/home/root/apps/remagic/share/testing/device-power-audit.sh" \
                "$audit_operation"
            ;;
        *)
            exec ssh "${USB_SSH_OPTIONS[@]}" root@remagic-device "$operation" "$@"
            ;;
    esac
}
