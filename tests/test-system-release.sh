#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
TMP=$(mktemp -d /tmp/remagic-system-release-test.XXXXXX)
trap 'rm -rf "$TMP"' EXIT HUP INT TERM

make_device() {
    root=$1
    machine=$2
    os=$3
    mkdir -p "$root/sys/devices/soc0" "$root/proc/device-tree" "$root/etc"
    printf '%s\n' "$machine" >"$root/sys/devices/soc0/machine"
    printf '%s\000' "$machine" >"$root/proc/device-tree/model"
    printf 'ID=codex\nVERSION_ID=5.7.126\nIMG_VERSION="%s"\n' "$os" \
        >"$root/etc/os-release"
}

make_device "$TMP/ferrari" 'reMarkable Ferrari' 3.27.3.0
output=$(
    REMAGIC_DETECT_ROOT="$TMP/ferrari"
    # shellcheck source=scripts/system-release/common.sh
    . "$ROOT/scripts/system-release/common.sh"
    detect_supported_device
    printf '%s|%s|%s\n' "$REMAGIC_DEVICE_PRODUCT" \
        "$REMAGIC_DEVICE_CODENAME" "$REMAGIC_OS_VERSION"
)
[ "$output" = 'paper_pro|ferrari|3.27.3.0' ]

make_device "$TMP/chiappa" 'reMarkable Chiappa' 3.27.9.1
output=$(
    REMAGIC_DETECT_ROOT="$TMP/chiappa"
    . "$ROOT/scripts/system-release/common.sh"
    detect_supported_device
    printf '%s|%s\n' "$REMAGIC_DEVICE_PRODUCT" "$REMAGIC_DEVICE_CODENAME"
)
[ "$output" = 'paper_pro_move|chiappa' ]

make_device "$TMP/old" 'reMarkable Chiappa' 3.26.4.0
if (
    REMAGIC_DETECT_ROOT="$TMP/old"
    . "$ROOT/scripts/system-release/common.sh"
    detect_supported_device
) 2>/dev/null; then
    echo "unsupported OS passed system preflight" >&2
    exit 1
fi

# remagicd becomes active before its control socket is ready. Verify that the
# release helper tolerates that window, succeeds on the first healthy probe,
# and still has a finite failure bound.
. "$ROOT/scripts/system-release/common.sh"
READY_ATTEMPTS=$TMP/ready-attempts
READY_CTL=$TMP/remagicctl
printf '0\n' >"$READY_ATTEMPTS"
cat >"$READY_CTL" <<EOF
#!/bin/sh
attempt=\$(cat "$READY_ATTEMPTS")
attempt=\$((attempt + 1))
printf '%s\n' "\$attempt" >"$READY_ATTEMPTS"
[ "\$attempt" -ge 3 ]
EOF
chmod 0755 "$READY_CTL"
systemctl() { return 0; }
wait_for_remagic_ready "$READY_CTL" 5 0
[ "$(cat "$READY_ATTEMPTS")" = 3 ] || {
    echo "manager readiness probe did not stop after success" >&2
    exit 1
}
printf '#!/bin/sh\nexit 1\n' >"$READY_CTL"
chmod 0755 "$READY_CTL"
if wait_for_remagic_ready "$READY_CTL" 2 0 >/dev/null 2>&1; then
    echo "manager readiness probe accepted an unavailable control socket" >&2
    exit 1
fi

make_device "$TMP/mismatch" 'reMarkable Ferrari' 3.27.3.0
printf 'reMarkable Chiappa\000' >"$TMP/mismatch/proc/device-tree/model"
if (
    REMAGIC_DETECT_ROOT="$TMP/mismatch"
    . "$ROOT/scripts/system-release/common.sh"
    detect_supported_device
) 2>/dev/null; then
    echo "conflicting machine identities passed preflight" >&2
    exit 1
fi

grep -q 'STORE_PACKAGE=packages/' "$ROOT/scripts/build-system-release.sh"
grep -Fq 'testing/manifests/$test_manifest' "$ROOT/scripts/build-system-release.sh" || {
    echo "system release does not ship isolated acceptance manifests" >&2
    exit 1
}
grep -Fq 'remagic-agentd.socket' "$ROOT/scripts/build-system-release.sh" || {
    echo "system release does not ship the Pi agent socket" >&2
    exit 1
}
grep -Fq 'BindsTo=remagicd.service' "$ROOT/systemd/remagic-agentd.service" || {
    echo "Pi agent broker is not lifecycle-bound to the manager" >&2
    exit 1
}
grep -Fq 'REMAGIC_API=5' "$ROOT/scripts/build-system-release.sh" || {
    echo "system release did not publish ReMagic API 5" >&2
    exit 1
}
grep -Fq 'REMAGIC_PI_RUNTIME_DIR must name a self-contained Pi runtime' \
    "$ROOT/scripts/build-system-release.sh" || {
    echo "system release still permits an unbundled Pi runtime" >&2
    exit 1
}
grep -Fq 'REMAGIC_PI_RUNTIME_SCHEMA=' "$ROOT/scripts/build-system-release.sh" || {
    echo "system release does not bind a Pi runtime version manifest" >&2
    exit 1
}
grep -Fq 'payload_pi_version' "$ROOT/scripts/system-release/install-device.sh" || {
    echo "system installer does not verify the bundled Pi runtime version" >&2
    exit 1
}
[ -s "$ROOT/runtime/pi/extensions/remagic-tools.js" ] || {
    echo "system release has no fixed safe Pi tools extension" >&2
    exit 1
}
[ -x "$ROOT/scripts/build-pi-runtime.sh" ] || {
    echo "system repository has no reproducible Pi runtime builder" >&2
    exit 1
}
[ -s "$ROOT/runtime/pi/package-lock.json" ] || {
    echo "Pi runtime dependencies are not locked" >&2
    exit 1
}
grep -Fq 'npm ci' "$ROOT/scripts/build-pi-runtime.sh" || {
    echo "Pi runtime builder does not consume its dependency lock" >&2
    exit 1
}
grep -Fq 'strip-unneeded "$PAYLOAD/runtime/pi/bin/node"' \
    "$ROOT/scripts/build-system-release.sh" || {
    echo "system release does not strip the packaged Node runtime" >&2
    exit 1
}
grep -Fq 'release-arm-state' "$ROOT/scripts/build-system-release.sh" || {
    echo "system release does not execute its ARM64 Pi payload" >&2
    exit 1
}
grep -Fq 'runtime/pi/extensions/remagic-tools.js' \
    "$ROOT/scripts/build-system-release.sh" || {
    echo "system release does not package the safe Pi tools extension" >&2
    exit 1
}
grep -Fq 'remagic-configure-provider' "$ROOT/scripts/build-system-release.sh" || {
    echo "system release does not package provider configuration support" >&2
    exit 1
}
grep -Fq 'REMAGIC_DEVICE_HOST:-${REMAGIC_HOST:-10.11.99.1}' \
    "$ROOT/configure-provider.sh" || {
    echo "provider helper does not share the installer host override" >&2
    exit 1
}
grep -Fq 'REMAGIC_SSH_TARGET:-root@$host' "$ROOT/configure-provider.sh" || {
    echo "provider helper does not share the installer SSH target override" >&2
    exit 1
}
if grep -Eq 'opt/magicpaper|opt/koreader-for-remagic|MAGICPAPER_DIR' \
    "$ROOT/scripts/build-system-release.sh"; then
    echo "system release still embeds a user application payload" >&2
    exit 1
fi
if grep -Eq 'rm -rf .*paperweight|rm -f .*paperweight' \
    "$ROOT/scripts/system-release/install-device.sh"; then
    echo "system installer can delete Paperweight files" >&2
    exit 1
fi
grep -Fq 'REMAGIC_STORE_CATALOG_DIR=$STORE_PAYLOAD/share/catalog' \
    "$ROOT/scripts/system-release/install-device.sh" || {
    echo "system installer does not seed the signed offline Store catalog" >&2
    exit 1
}
grep -Fq '"$STORE_PAYLOAD/bin/remagic-store" catalog' \
    "$ROOT/scripts/system-release/install-device.sh" || {
    echo "system installer does not verify the seeded Store catalog" >&2
    exit 1
}
grep -Fq 'wait_for_remagic_ready "$APP_ROOT/bin/remagicctl"' \
    "$ROOT/scripts/system-release/install-device.sh" || {
    echo "system installer does not wait for the manager control plane" >&2
    exit 1
}

for manifest in magicpaper koreader; do
    [ -s "$ROOT/testing/manifests/$manifest.toml" ] || {
        echo "missing isolated acceptance manifest: $manifest" >&2
        exit 1
    }
done

sh -n "$ROOT/install.sh"
sh -n "$ROOT/scripts/system-release/common.sh"
sh -n "$ROOT/scripts/system-release/install-device.sh"
sh -n "$ROOT/scripts/remagic-configure-provider"
bash -n "$ROOT/configure-provider.sh"
bash -n "$ROOT/scripts/build-pi-runtime.sh"
echo "system release contract passed"
