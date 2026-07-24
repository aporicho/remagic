#!/bin/sh
set -eu

HOST=${REMAGIC_HOST:-10.11.99.1}
SSH_TARGET=${REMAGIC_SSH_TARGET:-root@$HOST}
INDEX_URL=${REMAGIC_RELEASE_INDEX_URL:-https://github.com/aporicho/remagic/releases/latest/download/remagic-release.env}
USB_INTERFACE=${REMAGIC_USB_INTERFACE:-}
USB_PROXY=${REMAGIC_USB_PROXY:-}
USB_ALIAS=${REMAGIC_USB_ALIAS:-remagic-usb}
TMP=$(mktemp -d "${TMPDIR:-/tmp}/remagic-install.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

if [ -n "$USB_INTERFACE" ]; then
    case "$USB_INTERFACE" in
        *[!A-Za-z0-9_.:-]*|'')
            echo "ReMagic: invalid REMAGIC_USB_INTERFACE" >&2
            exit 1
            ;;
    esac
    [ -n "$USB_PROXY" ] && [ -x "$USB_PROXY" ] || {
        echo "ReMagic: REMAGIC_USB_PROXY must name an executable proxy" >&2
        exit 1
    }
    SSH_TARGET=${REMAGIC_SSH_TARGET:-root@remagic-device}
fi

device_ssh() {
    if [ -n "$USB_INTERFACE" ]; then
        ssh -F /dev/null \
            -o "HostName=$HOST" \
            -o "HostKeyAlias=$USB_ALIAS" \
            -o "ProxyCommand=$USB_PROXY $USB_INTERFACE %h %p" \
            -o ControlMaster=no -o ControlPath=none \
            -o StrictHostKeyChecking=accept-new "$@"
    else
        ssh -F /dev/null "$@"
    fi
}

device_scp() {
    if [ -n "$USB_INTERFACE" ]; then
        scp -F /dev/null \
            -o "HostName=$HOST" \
            -o "HostKeyAlias=$USB_ALIAS" \
            -o "ProxyCommand=$USB_PROXY $USB_INTERFACE %h %p" \
            -o ControlMaster=no -o ControlPath=none \
            -o StrictHostKeyChecking=accept-new "$@"
    else
        scp -F /dev/null "$@"
    fi
}

for command in curl ssh scp tar; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "ReMagic: missing required command: $command" >&2
        exit 1
    }
done
if command -v sha256sum >/dev/null 2>&1; then
    SHA_COMMAND=sha256sum
elif command -v shasum >/dev/null 2>&1; then
    SHA_COMMAND='shasum -a 256'
else
    echo "ReMagic: sha256sum or shasum is required" >&2
    exit 1
fi

echo "ReMagic: checking the connected tablet…"
identity=$(device_ssh -o BatchMode=yes -o ConnectTimeout=8 "$SSH_TARGET" '
    set -eu
    machine=$(tr -d "\000\r\n" </sys/devices/soc0/machine)
    model=$(tr -d "\000\r\n" </proc/device-tree/model)
    [ "$machine" = "$model" ] || exit 20
    os=$(sed -n "s/^IMG_VERSION=//p" /etc/os-release | sed -n "1{s/^\"//;s/\"$//;p;}")
    printf "%s|%s\n" "$machine" "$os"
') || {
    echo "ReMagic: cannot inspect the tablet over USB SSH ($SSH_TARGET)" >&2
    exit 1
}
machine=${identity%%|*}
os_version=${identity#*|}
case "$machine" in
    'reMarkable Ferrari') codename=ferrari; product='Paper Pro' ;;
    'reMarkable Chiappa') codename=chiappa; product='Paper Pro Move' ;;
    *) echo "ReMagic: unsupported tablet: $machine" >&2; exit 1 ;;
esac
case "$os_version" in
    3.27|3.27.*) ;;
    *)
        echo "ReMagic: software $os_version is not in the supported 3.27.x series" >&2
        exit 1
        ;;
esac

curl --fail --location --silent --show-error --retry 3 \
    --proto '=https' --tlsv1.2 "$INDEX_URL" -o "$TMP/release.env"
field() {
    sed -n "s/^$1=//p" "$TMP/release.env" | sed -n '1p'
}
schema=$(field REMAGIC_RELEASE_SCHEMA)
version=$(field REMAGIC_VERSION)
archive_url=$(field ARCHIVE_URL)
archive_sha=$(field ARCHIVE_SHA256)
devices=$(field SUPPORTED_DEVICES)
os_series=$(field SUPPORTED_OS_SERIES)
[ "$schema" = 1 ] && [ "$os_series" = 3.27 ] || {
    echo "ReMagic: unsupported release index" >&2
    exit 1
}
case "$version" in ''|*[!A-Za-z0-9._+-]*) echo "ReMagic: invalid version" >&2; exit 1 ;; esac
case "$archive_sha" in
    *[!0-9a-f]*|'') echo "ReMagic: invalid release SHA-256" >&2; exit 1 ;;
esac
[ "${#archive_sha}" -eq 64 ] || { echo "ReMagic: invalid SHA-256 length" >&2; exit 1; }
case ",$devices," in *,"$codename",*) ;; *) echo "ReMagic: release has no $product build" >&2; exit 1 ;; esac
case "$archive_url" in
    https://github.com/aporicho/remagic/releases/download/*/remagic-system-*-universal-aarch64.tar.gz) ;;
    *) echo "ReMagic: release asset URL is not trusted" >&2; exit 1 ;;
esac

archive=$TMP/remagic-system.tar.gz
echo "ReMagic: downloading $version for $product…"
curl --fail --location --silent --show-error --retry 3 \
    --proto '=https' --tlsv1.2 "$archive_url" -o "$archive"
actual=$($SHA_COMMAND "$archive" | awk '{print $1}')
[ "$actual" = "$archive_sha" ] || {
    echo "ReMagic: downloaded archive checksum mismatch" >&2
    exit 1
}

remote=/tmp/remagic-system-$version.tar.gz
echo "ReMagic: transferring the verified release…"
device_scp -O "$archive" "$SSH_TARGET:$remote"
device_ssh "$SSH_TARGET" "
    set -eu
    archive='$remote'
    expected='$archive_sha'
    actual=\$(sha256sum \"\$archive\" | awk '{print \$1}')
    [ \"\$actual\" = \"\$expected\" ]
    work=/tmp/remagic-system-install.\$\$
    trap 'rm -rf \"\$work\" \"\$archive\"' EXIT HUP INT TERM
    mkdir \"\$work\"
    tar -xzf \"\$archive\" -C \"\$work\"
    \"\$work/remagic-system/install-device.sh\"
"
echo "ReMagic $version is ready. Triple-press the power button to enter it."
