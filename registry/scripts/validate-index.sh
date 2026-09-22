#!/usr/bin/env bash
# Validate the registry index: signature verification via the same implementation as the client + schemaVersion assertion.
# Usage: validate-index.sh <index.json> [keyId=pubkey-hex,...]
# Public keys may be passed explicitly or injected via OPENCAPX_REGISTRY_OFFICIAL_KEYS; the binary can be overridden with OPENCAPX_BIN (default opencapx).
set -euo pipefail

INDEX="${1:?Usage: validate-index.sh <index.json> [keyId=pubkey-hex,...]}"
KEYS="${2:-${OPENCAPX_REGISTRY_OFFICIAL_KEYS:-}}"
if [[ -z "$KEYS" ]]; then
  echo "missing official public keys: pass the second argument, or export OPENCAPX_REGISTRY_OFFICIAL_KEYS" >&2
  exit 2
fi
BIN="${OPENCAPX_BIN:-opencapx}"

SCHEMA="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["schemaVersion"])' "$INDEX")"
if [[ "$SCHEMA" != "2" ]]; then
  echo "schemaVersion=$SCHEMA (expected 2)" >&2
  exit 1
fi

OPENCAPX_REGISTRY_OFFICIAL_KEYS="$KEYS" "$BIN" verify-index "$INDEX"
echo "OK: $INDEX (schemaVersion=2, signature verified)"
