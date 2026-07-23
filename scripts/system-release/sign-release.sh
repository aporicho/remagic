#!/usr/bin/env bash
set -euo pipefail

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
RELEASE=${1:?usage: sign-release.sh RELEASE_JSON [SIGNATURE_JSON]}
OUTPUT=${2:-${RELEASE%.json}.sig.json}
KEY=${REMAGIC_SYSTEM_SIGNING_KEY:?set REMAGIC_SYSTEM_SIGNING_KEY to the offline system Ed25519 key}
KEY_ID=${REMAGIC_SYSTEM_KEY_ID:-remagic-system-2026-01}
TMP=$(mktemp -d "${TMPDIR:-/tmp}/remagic-release-sign.XXXXXX")
trap 'rm -rf "$TMP"' EXIT

[ -f "$RELEASE" ] && [ -f "$KEY" ] || { echo "release or signing key is missing" >&2; exit 1; }
[ "$(stat -c %a "$KEY" 2>/dev/null || stat -f %Lp "$KEY")" = 600 ] || {
    echo "system signing key must have mode 0600" >&2
    exit 1
}
trusted=$(python3 - "$ROOT/system-trusted-keys.json" "$KEY_ID" <<'PY'
import json, sys
for key in json.load(open(sys.argv[1], encoding='utf-8'))['keys']:
    if key['key_id'] == sys.argv[2]:
        print(key['public_key'])
        break
PY
)
openssl pkey -in "$KEY" -pubout -outform DER -out "$TMP/public.der" >/dev/null 2>&1
public=$(tail -c 32 "$TMP/public.der" | openssl base64 -A)
[ "$public" = "$trusted" ] || { echo "system signing key does not match trusted key" >&2; exit 1; }
openssl pkeyutl -sign -rawin -inkey "$KEY" -in "$RELEASE" -out "$TMP/signature"
signature=$(openssl base64 -A -in "$TMP/signature")
digest=$(sha256sum "$RELEASE" | awk '{print $1}')
temporary=$OUTPUT.tmp.$$
python3 - "$temporary" "$KEY_ID" "$digest" "$signature" <<'PY'
import json, sys
json.dump({
    'schema': 1,
    'key_id': sys.argv[2],
    'algorithm': 'ed25519',
    'release_sha256': sys.argv[3],
    'signature': sys.argv[4],
}, open(sys.argv[1], 'w', encoding='utf-8'), indent=2)
open(sys.argv[1], 'a', encoding='utf-8').write('\n')
PY
chmod 0644 "$temporary"
mv -f "$temporary" "$OUTPUT"
echo "signed system release with $KEY_ID ($digest)"
