#!/usr/bin/env bash
# Signs OxideMerge.app and its .dmg for release (see docs/macos-signing.md).
#
#   macos-sign.sh setup        imports the Developer ID certificate into a temporary keychain
#   macos-sign.sh app  <.app>  signs the app, and checks the signature
#   macos-sign.sh dmg  <.dmg>  signs, notarizes and staples the disk image, and checks it
#   macos-sign.sh cleanup      deletes the temporary keychain and key files
#
# Signing with a Developer ID needs all of these environment variables; with none of them the
# app is signed ad hoc, as a community build without credentials, and the .dmg is only
# checked. Some but not all of them is an error, so a broken setup never ships ad hoc.
#
#   MACOS_CERTIFICATE_P12       base64 of the Developer ID Application certificate (.p12)
#   MACOS_CERTIFICATE_PASSWORD  its password
#   MACOS_NOTARY_KEY            base64 of the App Store Connect API key (.p8)
#   MACOS_NOTARY_KEY_ID         the key's ID
#   MACOS_NOTARY_ISSUER         the key's issuer ID
#
# No secret is ever printed: the script never traces its commands, and CI masks the values.
set -euo pipefail
umask 077

VARS=(MACOS_CERTIFICATE_P12 MACOS_CERTIFICATE_PASSWORD MACOS_NOTARY_KEY MACOS_NOTARY_KEY_ID
  MACOS_NOTARY_ISSUER)
WORK=${RUNNER_TEMP:-${TMPDIR:-/tmp}}/oxidemerge-signing
KEYCHAIN=$WORK/signing.keychain-db

die() {
  echo "::error::$*" >&2
  exit 1
}

# "developer-id" when every variable is set, "ad-hoc" when none is.
mode() {
  local set=0 v
  for v in "${VARS[@]}"; do
    [ -n "${!v:-}" ] && set=$((set + 1))
  done
  if [ "$set" -eq "${#VARS[@]}" ]; then
    echo developer-id
  elif [ "$set" -eq 0 ]; then
    echo ad-hoc
  else
    die "Only $set of the ${#VARS[@]} macOS signing secrets are set; set all of them or none"
  fi
}

# What codesign says about a signature; read whole so `grep -q` cannot cut the pipe short.
signature() {
  codesign -d --verbose=2 "$1" 2>&1
}

identity() {
  security find-identity -v -p codesigning "$KEYCHAIN" |
    sed -n 's/.*"\(Developer ID Application: .*\)"$/\1/p' | head -n 1
}

setup() {
  [ "$(mode)" = developer-id ] || {
    echo "No signing secrets: the app will be signed ad hoc."
    return
  }
  mkdir -p "$WORK"
  local pass
  pass=$(openssl rand -hex 24)
  base64 --decode >"$WORK/cert.p12" <<<"$MACOS_CERTIFICATE_P12"
  base64 --decode >"$WORK/notary.p8" <<<"$MACOS_NOTARY_KEY"
  security create-keychain -p "$pass" "$KEYCHAIN"
  security set-keychain-settings -lut 3600 "$KEYCHAIN"
  security unlock-keychain -p "$pass" "$KEYCHAIN"
  security import "$WORK/cert.p12" -k "$KEYCHAIN" -P "$MACOS_CERTIFICATE_PASSWORD" \
    -T /usr/bin/codesign >/dev/null
  rm -f "$WORK/cert.p12"
  security set-key-partition-list -S apple-tool:,apple: -s -k "$pass" "$KEYCHAIN" >/dev/null
  # codesign finds the identity only in a keychain on the search list.
  local list
  list=$(security list-keychains -d user | tr -d '"')
  # shellcheck disable=SC2086
  security list-keychains -d user -s "$KEYCHAIN" $list
  [ -n "$(identity)" ] || die "The certificate holds no Developer ID Application identity"
  echo "Signing as: $(identity)"
}

sign_app() {
  local app=$1 bin
  [ -d "$app" ] || die "$app is not an app bundle"
  bin="$app/Contents/MacOS/oxidemerge"
  if [ "$(mode)" = developer-id ]; then
    # From the inside out, with the hardened runtime and a secure timestamp; no --deep.
    codesign --force --options runtime --timestamp --keychain "$KEYCHAIN" \
      --sign "$(identity)" "$bin"
    codesign --force --options runtime --timestamp --keychain "$KEYCHAIN" \
      --sign "$(identity)" "$app"
    codesign --verify --strict --verbose=2 "$app"
    local info
    info=$(signature "$app")
    grep -q '^Authority=Developer ID Application' <<<"$info" ||
      die "$app is not signed with a Developer ID"
    grep -q 'flags=.*runtime' <<<"$info" ||
      die "$app is not signed with the hardened runtime"
  else
    # Apple silicon refuses to run unsigned code; an ad hoc signature is enough to run.
    codesign --force --sign - "$bin"
    codesign --force --sign - "$app"
    codesign --verify --strict --verbose=2 "$app"
    grep -q '^Signature=adhoc' <<<"$(signature "$app")" ||
      die "$app has no ad hoc signature"
  fi
}

sign_dmg() {
  local dmg=$1
  [ -f "$dmg" ] || die "$dmg does not exist"
  if [ "$(mode)" = developer-id ]; then
    codesign --force --timestamp --keychain "$KEYCHAIN" --sign "$(identity)" "$dmg"
    xcrun notarytool submit "$dmg" --wait --timeout 30m \
      --key "$WORK/notary.p8" --key-id "$MACOS_NOTARY_KEY_ID" --issuer "$MACOS_NOTARY_ISSUER"
    xcrun stapler staple "$dmg"
    xcrun stapler validate "$dmg"
    local verdict
    verdict=$(spctl --assess --type open --context context:primary-signature -vv "$dmg" 2>&1) || true
    echo "$verdict"
    grep -q 'source=Notarized Developer ID' <<<"$verdict" ||
      die "Gatekeeper does not accept $dmg as notarized"
    # The app inside, as Gatekeeper sees it once copied out.
    local mnt
    mnt=$(mktemp -d)
    hdiutil attach -nobrowse -readonly -mountpoint "$mnt" "$dmg" >/dev/null
    verdict=$(spctl --assess --type execute -vv "$mnt/OxideMerge.app" 2>&1) || true
    hdiutil detach "$mnt" >/dev/null
    echo "$verdict"
    grep -q 'source=Notarized Developer ID' <<<"$verdict" || die "Gatekeeper does not accept the app in $dmg as notarized"
  fi
  hdiutil verify "$dmg" >/dev/null || die "$dmg fails hdiutil verify"
}

cleanup() {
  if [ -f "$KEYCHAIN" ]; then
    security delete-keychain "$KEYCHAIN" || true
  fi
  rm -rf "$WORK"
}

case "${1:-}" in
setup) setup ;;
app) sign_app "${2:?usage: $0 app <path.app>}" ;;
dmg) sign_dmg "${2:?usage: $0 dmg <path.dmg>}" ;;
cleanup) cleanup ;;
mode) mode ;;
*) die "usage: $0 setup | app <path.app> | dmg <path.dmg> | cleanup | mode" ;;
esac
