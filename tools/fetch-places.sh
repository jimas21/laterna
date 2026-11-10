#!/usr/bin/env bash
# Downloads the place packs pinned in packaging/places.lock into OUTDIR/<id>/.
#
# Usage: tools/fetch-places.sh OUTDIR [LOCKFILE]
#
# Each lock line is "<id> <url> <sha256>" (blank lines and # comments are
# skipped). The zip must match the sha256, hold only files inside "<id>/" (no
# absolute path, no "..", no links) and a "<id>/manifest.json". Any failure
# stops the script with a non-zero status. Only https urls are fetched, except
# when LATERNA_FETCH_ALLOW_FILE=1 (for tests, which use file:// urls).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
[ $# -ge 1 ] && [ $# -le 2 ] || { echo "usage: $0 OUTDIR [LOCKFILE]" >&2; exit 2; }
out="$1"
lock="${2:-$root/packaging/places.lock}"
[ -f "$lock" ] || { echo "fetch-places: no lock file $lock" >&2; exit 1; }
proto='=https'
[ "${LATERNA_FETCH_ALLOW_FILE:-0}" = 1 ] && proto='=https,file'

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"
n=0
while read -r id url sha extra || [ -n "${id:-}" ]; do
  case "${id:-}" in '' | '#'*) continue ;; esac
  if [ -z "${url:-}" ] || [ -z "${sha:-}" ] || [ -n "${extra:-}" ]; then
    echo "fetch-places: bad line for '$id' (want: id url sha256)" >&2
    exit 1
  fi
  [[ "$id" =~ ^[a-z0-9][a-z0-9._-]*$ ]] || { echo "fetch-places: bad pack id '$id'" >&2; exit 1; }
  [[ "$sha" =~ ^[0-9a-f]{64}$ ]] || { echo "fetch-places: $id: sha256 must be 64 lower-case hex digits" >&2; exit 1; }

  zip="$work/$id.zip"
  echo "== fetching $id"
  curl -fL --proto "$proto" --proto-redir "$proto" --retry 5 --retry-delay 3 --retry-all-errors \
    --connect-timeout 30 -sS -o "$zip" "$url"
  got="$(sha256sum "$zip" | cut -d' ' -f1)"
  if [ "$got" != "$sha" ]; then
    echo "fetch-places: $id: sha256 mismatch (lock $sha, got $got)" >&2
    exit 1
  fi

  # Every entry must sit inside "<id>/", with a plain relative name.
  entries="$(unzip -Z1 "$zip")"
  [ -n "$entries" ] || { echo "fetch-places: $id: empty zip" >&2; exit 1; }
  while IFS= read -r name; do
    case "$name" in
      "$id"/*) ;;
      *) echo "fetch-places: $id: entry '$name' is outside $id/" >&2; exit 1 ;;
    esac
    case "$name" in
      /* | *\\* | */../* | */.. | */./* | *//*)
        echo "fetch-places: $id: unsafe entry name '$name'" >&2
        exit 1
        ;;
    esac
  done <<<"$entries"
  if unzip -Z "$zip" | grep -q '^l'; then
    echo "fetch-places: $id: zip holds a symbolic link" >&2
    exit 1
  fi
  grep -qx "$id/manifest.json" <<<"$entries" || { echo "fetch-places: $id: no $id/manifest.json" >&2; exit 1; }

  rm -rf "$work/x" && mkdir "$work/x"
  unzip -q "$zip" -d "$work/x"
  [ -d "$work/x/$id" ] || { echo "fetch-places: $id: nothing unpacked" >&2; exit 1; }
  rm -rf "${out:?}/$id"
  mv "$work/x/$id" "$out/$id"
  echo "   $id: $(find "$out/$id" -type f | wc -l) files, $(du -sk "$out/$id" | cut -f1) KiB, zip sha256 $got"
  n=$((n + 1))
done <"$lock"
echo "fetch-places: $n pack(s) in $out"
