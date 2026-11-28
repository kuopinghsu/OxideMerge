#!/usr/bin/env bash
# Checks that every release package in a folder is a whole, readable file of its kind, then
# writes SHA256SUMS there and checks the packages against it.
#
#   check-release.sh <dist>
#
# A .zip or .tar.gz must list and decompress without error, an .AppImage must be an ELF
# program with the AppImage type 2 mark, an .AppImage.zsync must be a zsync control file that
# points to the AppImage beside it and gives its size and SHA-1, a .dmg must end with its "koly"
# trailer, and a .pdf must start with %PDF- and end with %%EOF. Any other file fails the check.
# This catches truncated or damaged uploads; it does not check signatures (see
# docs/macos-signing.md).
set -euo pipefail

dist=${1:?usage: $0 <dist>}
cd "$dist"

bad=0
fail() {
  echo "::error::$1: $2" >&2
  bad=1
}

# The SHA-1 digest of $1, as zsync writes it: lowercase hex. A missing tool stops the check
# here, rather than making every .zsync look damaged.
if command -v sha1sum >/dev/null; then
  sha1_tool=(sha1sum)
elif command -v shasum >/dev/null; then
  sha1_tool=(shasum -a 1)
else
  echo "::error::check-release.sh needs sha1sum or shasum" >&2
  exit 1
fi
sha1() {
  "${sha1_tool[@]}" "$1" | cut -d ' ' -f 1
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
  *.AppImage.zsync)
    # The header is text up to its first empty line; the block checksums follow it. sed reads
    # everything head gives it, so neither stops early under pipefail.
    header=$(head -c 4096 "$f" | LC_ALL=C sed '/^\r*$/,$d' | tr -d '\r')
    image=${f%.zsync}
    if [[ $header != "zsync: "* ]]; then
      fail "$f" "is not a zsync control file"
    elif ! grep -qxF "URL: $image" <<<"$header"; then
      fail "$f" "does not point to $image beside it"
    elif [ ! -f "$image" ]; then
      fail "$f" "has no $image beside it"
    elif ! grep -qxF "Length: $(wc -c <"$image" | tr -d ' ')" <<<"$header" ||
      ! grep -qxF "SHA-1: $(sha1 "$image")" <<<"$header"; then
      fail "$f" "does not describe $image as it is"
    fi
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
