#!/usr/bin/env bash
# Checks that every release package in a folder is a whole, readable file of its kind, then
# writes SHA256SUMS there and checks the packages against it.
#
#   check-release.sh <dist>
#
# A .zip or .tar.gz must list and decompress without error, an .AppImage must be an ELF
# program with the AppImage type 2 mark, a .dmg must end with its "koly" trailer, and a .pdf
# must start with %PDF- and end with %%EOF. Any other file fails the check. This catches
# truncated or damaged uploads; it does not check signatures (see docs/macos-signing.md).
set -euo pipefail

dist=${1:?usage: $0 <dist>}
cd "$dist"

bad=0
fail() {
  echo "::error::$1: $2" >&2
  bad=1
}

# The bytes of $1 from offset $2, $3 of them, as hex.
bytes() {
  od -An -tx1 -j "$2" -N "$3" "$1" | tr -d ' \n'
}

shopt -s nullglob
files=()
for f in *; do
  [ "$f" = SHA256SUMS ] && continue
  [ -f "$f" ] || continue
  files+=("$f")
  [ -s "$f" ] || {
    fail "$f" "is empty"
    continue
  }
  case "$f" in
  *.zip)
    unzip -tq "$f" >/dev/null 2>&1 || fail "$f" "is not a readable zip archive"
    ;;
  *.tar.gz)
    { gzip -t "$f" && tar -tzf "$f" >/dev/null; } 2>/dev/null ||
      fail "$f" "is not a readable .tar.gz archive"
    ;;
  *.AppImage)
    [ "$(bytes "$f" 0 4)" = 7f454c46 ] || fail "$f" "is not an ELF program"
    [ "$(bytes "$f" 8 3)" = 414902 ] || fail "$f" "has no AppImage type 2 mark"
    ;;
  *.dmg)
    size=$(wc -c <"$f")
    if [ "$size" -lt 512 ]; then
      fail "$f" "is too small for a disk image"
    else
      [ "$(bytes "$f" $((size - 512)) 4)" = 6b6f6c79 ] || fail "$f" "has no disk image trailer"
    fi
    ;;
  *.pdf)
    [ "$(head -c 5 "$f")" = "%PDF-" ] || fail "$f" "does not start as a PDF"
    tail -c 1024 "$f" | grep -aq '%%EOF' || fail "$f" "does not end as a PDF"
    ;;
  *)
    fail "$f" "is not a known kind of release file"
    ;;
  esac
done

[ "${#files[@]}" -gt 0 ] || fail "$dist" "holds no release files"
[ "$bad" = 0 ] || exit 1

if command -v sha256sum >/dev/null; then
  sum=(sha256sum)
else
  sum=(shasum -a 256)
fi
"${sum[@]}" "${files[@]}" >SHA256SUMS
"${sum[@]}" -c SHA256SUMS
echo "Checked ${#files[@]} release files; digests in $dist/SHA256SUMS"
