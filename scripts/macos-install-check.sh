#!/usr/bin/env bash
# Installs OxideMerge from its .dmg the way the README says, as a download would arrive, and
# removes it again, checking each step:
#
#   scripts/macos-install-check.sh <oxidemerge.dmg> [ad-hoc|developer-id]
#
# 1. The disk image verifies and holds OxideMerge.app and an Applications link.
# 2. The app is copied to an Applications folder of its own (never /Applications) and given
#    the quarantine flag a browser download gets.
# 3. Gatekeeper's verdict on it (`spctl --assess`) is the one expected: rejected for an ad hoc
#    build, the reason the README tells users to choose Open Anyway; accepted as notarized for
#    a Developer ID build.
# 4. For an ad hoc build, the README's workaround (`xattr -dr com.apple.quarantine`) clears
#    the flag.
# 5. The installed program runs and compares files, with settings in a folder of its own.
# 6. Removing it as the README says leaves nothing of it behind: no file of its own in the
#    Applications folder, and nothing new named after it in ~/Library or the temporary folder.
#
# Where Gatekeeper is turned off its verdict means nothing, and the check says so and goes on.
# With INSTALL_CHECK_GATEKEEPER=require (as in CI) it turns Gatekeeper on first (sudo -n), and
# fails if it cannot, rather than passing without having checked.
#
# This is not a clean machine: Gatekeeper's first-launch dialog and the Open Anyway button
# cannot be clicked from a script, so that part stays a manual check before a release.
set -euo pipefail

dmg=${1:?usage: $0 <oxidemerge.dmg> [ad-hoc|developer-id]}
expect=${2:-ad-hoc}
case "$expect" in ad-hoc | developer-id) ;; *) echo "unknown signing: $expect" >&2; exit 2 ;; esac
dmg=$(cd "$(dirname "$dmg")" && pwd)/$(basename "$dmg")
here=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
mnt=$work/mnt
cleanup() {
  hdiutil detach "$mnt" -quiet 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
fail() {
  echo "::error::install check: $*" >&2
  exit 1
}

echo "1. disk image"
hdiutil verify -quiet "$dmg" || fail "$dmg does not verify"
mkdir -p "$mnt"
hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "$dmg" >/dev/null
[ -d "$mnt/OxideMerge.app" ] || fail "no OxideMerge.app in the disk image"
[ "$(readlink "$mnt/Applications")" = /Applications ] || fail "no Applications link in the disk image"

echo "2. copied to an Applications folder, as downloaded"
apps=$work/Applications
mkdir -p "$apps"
ditto "$mnt/OxideMerge.app" "$apps/OxideMerge.app"
hdiutil detach "$mnt" -quiet
app=$apps/OxideMerge.app
# The flag a browser sets: flags, time, agent, and an identifier.
xattr -w com.apple.quarantine "0083;$(printf '%x' "$(date +%s)");Safari;" "$app"
xattr -p com.apple.quarantine "$app" >/dev/null || fail "the quarantine flag was not set"

echo "3. Gatekeeper"
# Read whole into a string, so that `grep -q` cannot cut the pipe short under pipefail.
gatekeeper_off() { grep -q disabled <<<"$(spctl --status 2>&1)"; }
if gatekeeper_off && [ "${INSTALL_CHECK_GATEKEEPER:-}" = require ]; then
  echo "   Gatekeeper is turned off; turning it on"
  sudo -n spctl --master-enable 2>/dev/null || sudo -n spctl --global-enable 2>/dev/null || true
  ! gatekeeper_off || fail "Gatekeeper is turned off and could not be turned on, so its verdict cannot be checked"
fi
verdict=$(spctl --assess --type execute -vv "$app" 2>&1) && accepted=1 || accepted=0
echo "$verdict" | sed 's/^/   /'
if grep -q 'override=security disabled' <<<"$verdict" || gatekeeper_off; then
  # With assessments turned off (spctl --master-disable) every app is accepted, so the
  # verdict says nothing about the build. Say so rather than pass or fail on it.
  echo "::warning::install check: Gatekeeper is turned off on this Mac; its verdict was not checked"
  gatekeeper=off
else
  gatekeeper=on
fi
if [ "$gatekeeper" = off ]; then
  [ "$expect" = developer-id ] || grep -q "^Signature=adhoc" <<<"$(codesign -dv "$app" 2>&1)" ||
    fail "the app is not signed ad hoc"
elif [ "$expect" = developer-id ]; then
  [ "$accepted" = 1 ] && grep -q 'source=Notarized Developer ID' <<<"$verdict" ||
    fail "Gatekeeper does not accept the app as notarized"
else
  [ "$accepted" = 0 ] || fail "Gatekeeper accepts an ad hoc app; the README's Open Anyway steps would be wrong"
  grep -q "^Signature=adhoc" <<<"$(codesign -dv "$app" 2>&1)" || fail "the app is not signed ad hoc"
fi
if [ "$expect" = ad-hoc ]; then
  echo "4. the README's workaround"
  xattr -dr com.apple.quarantine "$app"
  if xattr -p com.apple.quarantine "$app" >/dev/null 2>&1; then
    fail "xattr -dr did not clear the quarantine flag"
  fi
fi

echo "5. the installed program"
# Anything of OxideMerge's written from here on is newer than this file.
since=$work/since
touch "$since"
bin=$app/Contents/MacOS/oxidemerge
export OXIDEMERGE_CONFIG_DIR=$work/settings
"$bin" --version
"$bin" --cli "$here/samples/left/same.txt" "$here/samples/right/same.txt"
code=0
"$bin" --cli -r "$here/samples/left" "$here/samples/right" >/dev/null || code=$?
[ "$code" -eq 1 ] || fail "differing folders gave exit code $code, not 1"
codesign --verify --strict "$app" || fail "the installed app's signature does not verify"

echo "6. removed"
# As the README says: the app, then its settings folder (here the one it was given).
rm -rf "$app" "$OXIDEMERGE_CONFIG_DIR"
[ -z "$(ls -A "$apps")" ] || fail "something was left in the Applications folder: $(ls -A "$apps")"
# What the program may have written past the settings folder it was given: preferences,
# caches, saved window state, settings, or temporary folders of its own.
places=()
for d in "$HOME/Library/Application Support" "$HOME/Library/Caches" "$HOME/Library/Preferences" \
  "$HOME/Library/Saved Application State" "${TMPDIR:-/tmp}"; do
  [ -d "$d" ] && places+=("$d")
done
left=$(find "${places[@]}" -maxdepth 1 -newer "$since" -iname '*oxidemerge*' 2>/dev/null || true)
[ -z "$left" ] || fail "the program left files outside its settings folder: $left"
echo "install check: ok ($expect)"
