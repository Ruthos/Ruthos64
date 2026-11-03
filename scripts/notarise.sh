#!/usr/bin/env bash
# Submit a zip or disk image to Apple's notary service and wait. Fails on
# any status other than Accepted and prints the notary log.
# Needs API_KEY_PATH, APPLE_API_KEY_ID and APPLE_API_ISSUER (release.yml).
set -euo pipefail
file="$1"
out=$(xcrun notarytool submit "$file" \
  --key "$API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" \
  --wait --output-format json)
echo "$out"
id=$(echo "$out" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
status=$(echo "$out" | /usr/bin/python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])')
if [ "$status" != "Accepted" ]; then
  echo "Notarisation of $file: $status"
  xcrun notarytool log "$id" \
    --key "$API_KEY_PATH" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER" || true
  exit 1
fi
echo "Notarised: $file"
