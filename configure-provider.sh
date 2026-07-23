#!/usr/bin/env bash
set -euo pipefail

provider=${1:-deepseek}
host=${REMAGIC_DEVICE_HOST:-${REMAGIC_HOST:-10.11.99.1}}
ssh_target=${REMAGIC_SSH_TARGET:-root@$host}
case $provider in
    deepseek|openai) ;;
    *) echo "usage: $0 [deepseek|openai]" >&2; exit 2 ;;
esac
read -rsp "${provider} API key: " secret
printf '\n' >&2
[ -n "$secret" ] || { echo "API key may not be empty" >&2; exit 2; }
[ "$(printf '%s' "$secret" | wc -c)" -le 16384 ] || {
    echo "API key exceeds 16 KiB" >&2
    exit 2
}
read -rp "Optional API base URL (blank for provider default): " base_url
[ "$(printf '%s' "$base_url" | wc -c)" -le 2048 ] || {
    echo "BASE_URL exceeds 2048 bytes" >&2
    exit 2
}
case $base_url in
    ''|http://*|https://*) ;;
    *) echo "BASE_URL must use http or https" >&2; exit 2 ;;
esac
if [[ $base_url =~ [[:cntrl:][:space:]] ]]; then
    echo "BASE_URL contains invalid characters" >&2
    exit 2
fi
printf '%s\n%s\n' "$secret" "$base_url" | \
    ssh -F /dev/null "$ssh_target" \
        "/home/root/apps/remagic/libexec/remagic-configure-provider '$provider'"
unset secret
