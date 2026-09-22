#!/usr/bin/env bash
# Pack a plugin directory into a .ocplugin (ZIP, manifest at the package root).
# Usage: pack-ocplugin.sh <plugin-dir> [out.ocplugin] [--sign <seed|@file> --key-id <id>]
# WHY: with --sign/--key-id → delegate signing to the Python SDK (Ed25519 v2), and point PYTHONPATH at
# this repo's packages/plugin-sdk so authors do not need to pip install first; without them, behavior is byte-for-byte identical to the old version (plain packing).
set -euo pipefail

sign_arg=""
key_id=""
positional=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --sign) sign_arg=${2:?pack-ocplugin.sh: --sign requires a value}; shift 2 ;;
    --key-id) key_id=${2:?pack-ocplugin.sh: --key-id requires a value}; shift 2 ;;
    --) shift; positional+=("$@"); break ;;
    *) positional+=("$1"); shift ;;
  esac
done

if [[ ${#positional[@]} -eq 0 ]]; then
  echo "usage: pack-ocplugin.sh <plugin-dir> [out.ocplugin] [--sign <seed|@file> --key-id <id>]" >&2
  exit 1
fi

dir=${positional[0]%/}
[[ -f "$dir/opencapx-plugin.json" ]] || { echo "missing opencapx-plugin.json in $dir" >&2; exit 1; }

id=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["id"])' "$dir/opencapx-plugin.json")
version=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["version"])' "$dir/opencapx-plugin.json")
out=${positional[1]:-"${id}-${version}.ocplugin"}

if [[ -n "$sign_arg" || -n "$key_id" ]]; then
  if [[ -z "$sign_arg" || -z "$key_id" ]]; then
    echo "pack-ocplugin.sh: --sign and --key-id must be provided together" >&2
    exit 1
  fi
  repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
  PYTHONPATH="$repo_root/packages/plugin-sdk${PYTHONPATH:+:$PYTHONPATH}" \
    python3 -m opencapx_sdk.signing pack "$dir" \
      --key "$sign_arg" --key-id "$key_id" --out "$out" >/dev/null
  echo "$out"
  exit 0
fi

python3 - "$dir" "$out" <<'EOF'
import os, sys, zipfile
src, out = sys.argv[1], sys.argv[2]
# Development artifacts stay out of the package: test caches/bytecode/virtualenvs/repos. Previously .pytest_cache was packed
# into the .ocplugin as-is (nodeids stuffed into a 95KB demo package), both bloated and drifting with test results.
SKIP_DIRS = {".git", ".pytest_cache", "__pycache__", ".venv", ".mypy_cache", ".ruff_cache", "node_modules"}
SKIP_FILES = {".DS_Store"}
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    for root, dirs, files in os.walk(src):
        dirs[:] = sorted(d for d in dirs if d not in SKIP_DIRS)
        for f in sorted(files):
            if f in SKIP_FILES or f.endswith((".pyc", ".pyo")):
                continue
            full = os.path.join(root, f)
            z.write(full, os.path.relpath(full, src))
print(out)
EOF
