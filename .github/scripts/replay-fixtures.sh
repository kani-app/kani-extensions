#!/usr/bin/env bash
# Replays every recorded response under tests/<id>/<endpoint>.har through the extension and
# compares the rows with tests/<id>/<endpoint>.expected.json. Offline: hooks have already run
# on a recorded body, so this covers extraction, not request signing or live site changes.
# Usage: replay-fixtures.sh <kani-cli>
set -euo pipefail
cli="$1"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
sources="$(python3 "$root/.github/scripts/extensions.py" list)"
failed=0
shopt -s nullglob
for har in "$root"/tests/*/*.har; do
  id="$(basename "$(dirname "$har")")"
  endpoint="$(basename "$har" .har)"
  expected="${har%.har}.expected.json"
  source="$(jq -r --arg id "$id" '.[] | select(.id == $id) | "\(.format) \(.source)"' <<<"$sources")"
  case "$source" in
    "yaml "*) ;;
    "") echo "::error::$har: no extension has id '$id'"; failed=1; continue ;;
    *) echo "::error::$har: replay supports YAML extensions only"; failed=1; continue ;;
  esac
  [ -f "$expected" ] || { echo "::error::$har has no ${expected##*/}"; failed=1; continue; }
  echo "==> $id $endpoint"
  if ! "$cli" repl replay "$root/${source#yaml }" "$har" "$endpoint" "$expected"; then
    failed=1
  fi
done
exit "$failed"
