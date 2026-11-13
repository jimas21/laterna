#!/usr/bin/env bash
# Tests tools/fetch-places.sh with local zips and file:// urls (no network):
# a good pin bundles, a wrong sha256 is refused, a zip with a "../" or an
# absolute entry is refused, an empty lock bundles nothing.
#
# Usage: tools/test-fetch-places.sh        (needs python3, unzip, sha256sum)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
fetch="$here/fetch-places.sh"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export LATERNA_FETCH_ALLOW_FILE=1
fails=0

# mkzip OUT ENTRY...  writes a zip whose entries are the given names (each a
# small text file), even names that zip(1) would not accept.
mkzip() {
  python3 - "$@" <<'PY'
import sys
import zipfile

out, names = sys.argv[1], sys.argv[2:]
with zipfile.ZipFile(out, "w") as z:
    for n in names:
        z.writestr(n, "x\n")
PY
}

# lockfor ID ZIP [SHA]  writes the lock file for one pack to $work/lock.
lockfor() {
  local sha="${3:-$(sha256sum "$2" | cut -d' ' -f1)}"
  printf '# test lock\n%s file://%s %s\n' "$1" "$2" "$sha" >"$work/lock"
}

# expect_ok NAME OUTDIR   expect_refused NAME OUTDIR PATTERN
expect_ok() {
  if "$fetch" "$2" "$work/lock" >"$work/log" 2>&1; then
    echo "ok   $1"
  else
    echo "FAIL $1: refused" && sed 's/^/     /' "$work/log" && fails=$((fails + 1))
  fi
}
expect_refused() {
  if "$fetch" "$2" "$work/lock" >"$work/log" 2>&1; then
    echo "FAIL $1: accepted" && fails=$((fails + 1))
  elif grep -q -- "$3" "$work/log"; then
    echo "ok   $1"
  else
    echo "FAIL $1: refused for another reason" && sed 's/^/     /' "$work/log" && fails=$((fails + 1))
  fi
}

# A good pin bundles its files into OUT/<id>/.
mkzip "$work/good.zip" good/manifest.json good/mesh.bin good/sub/tex.ktx2
lockfor good "$work/good.zip"
expect_ok "a good pin bundles" "$work/out1"
for f in manifest.json mesh.bin sub/tex.ktx2; do
  [ -f "$work/out1/good/$f" ] || { echo "FAIL a good pin bundles: $f missing"; fails=$((fails + 1)); }
done

# The wrong sha256 is refused and nothing is bundled.
lockfor good "$work/good.zip" "$(printf '0%.0s' {1..64})"
expect_refused "a wrong sha256 is refused" "$work/out2" "sha256 mismatch"
[ ! -e "$work/out2/good" ] || { echo "FAIL a wrong sha256 left files behind"; fails=$((fails + 1)); }

# A zip entry that climbs out with "..", or is absolute, is refused.
mkzip "$work/dots.zip" evil/manifest.json evil/../../escape.txt
lockfor evil "$work/dots.zip"
expect_refused "a ../ entry is refused" "$work/out3" "outside\|unsafe"
mkzip "$work/dots2.zip" evil/manifest.json evil/a/../../b.txt
lockfor evil "$work/dots2.zip"
expect_refused "a ../ entry inside the pack folder is refused" "$work/out3b" "unsafe"
mkzip "$work/abs.zip" evil/manifest.json /tmp/laterna-abs.txt
lockfor evil "$work/abs.zip"
expect_refused "an absolute entry is refused" "$work/out4" "outside\|unsafe"
[ ! -e /tmp/laterna-abs.txt ] || { echo "FAIL an absolute entry was written"; fails=$((fails + 1)); }
[ ! -e "$work/escape.txt" ] || { echo "FAIL a ../ entry was written"; fails=$((fails + 1)); }

# A pack without its manifest is refused.
mkzip "$work/nomanifest.zip" bare/mesh.bin
lockfor bare "$work/nomanifest.zip"
expect_refused "a pack with no manifest is refused" "$work/out5" "manifest.json"

# A file:// url is refused unless the test switch is on.
lockfor good "$work/good.zip"
if LATERNA_FETCH_ALLOW_FILE=0 "$fetch" "$work/out6" "$work/lock" >"$work/log" 2>&1; then
  echo "FAIL file:// accepted without the switch"
  fails=$((fails + 1))
else
  echo "ok   file:// is refused without the switch"
fi

# An empty lock (only comments and blank lines) bundles nothing.
printf '# nothing pinned\n\n' >"$work/lock"
expect_ok "an empty lock is fine" "$work/out7"
if [ -n "$(ls -A "$work/out7")" ]; then
  echo "FAIL an empty lock bundled something"
  fails=$((fails + 1))
fi

if [ "$fails" -ne 0 ]; then
  echo "test-fetch-places: $fails failure(s)" >&2
  exit 1
fi
echo "test-fetch-places: all passed"
