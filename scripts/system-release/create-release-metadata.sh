#!/usr/bin/env bash
set -euo pipefail

ARCHIVE=${1:?usage: create-release-metadata.sh ARCHIVE VERSION SEQUENCE OUTPUT_JSON}
VERSION=${2:?usage: create-release-metadata.sh ARCHIVE VERSION SEQUENCE OUTPUT_JSON}
SEQUENCE=${3:?usage: create-release-metadata.sh ARCHIVE VERSION SEQUENCE OUTPUT_JSON}
OUTPUT=${4:?usage: create-release-metadata.sh ARCHIVE VERSION SEQUENCE OUTPUT_JSON}
ARCHIVE_URL=${REMAGIC_ARCHIVE_URL:-https://github.com/aporicho/remagic/releases/download/v${VERSION}/$(basename "$ARCHIVE")}
NOW=${REMAGIC_RELEASE_NOW:-$(date +%s)}
EXPIRES=${REMAGIC_RELEASE_EXPIRES:-$((NOW + 31536000))}

[ -f "$ARCHIVE" ] || { echo "system archive is missing" >&2; exit 1; }
size=$(stat -c %s "$ARCHIVE" 2>/dev/null || stat -f %z "$ARCHIVE")
sha=$(sha256sum "$ARCHIVE" | awk '{print $1}')
python3 - "$OUTPUT" "$VERSION" "$SEQUENCE" "$ARCHIVE_URL" "$size" "$sha" "$NOW" "$EXPIRES" <<'PY'
import json, sys
out, version, sequence, url, size, sha, now, expires = sys.argv[1:]
doc = {
    "schema": 1,
    "release_id": f"remagic-{version}",
    "version": version,
    "sequence": int(sequence),
    "supported_devices": ["paper_pro", "paper_pro_move"],
    "supported_os": ["3.27"],
    "required_remagic_api": 5,
    "requires_reboot": False,
    "archive": {"url": url, "sha256": sha, "size_bytes": int(size)},
    "generated_at_unix": int(now),
    "expires_at_unix": int(expires),
}
json.dump(doc, open(out, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
open(out, "a", encoding="utf-8").write("\n")
PY
echo "created system release metadata: $OUTPUT"
