# macOS signing and notarization

OxideMerge for macOS is a universal `OxideMerge.app` (Apple silicon and Intel) shipped in
a `.dmg`. It is signed ad hoc and is not notarized. This note explains what that means for
users, what notarization would take, and why it hasn't been adopted yet.

## What users see

A downloaded app carries the `com.apple.quarantine` flag, and Gatekeeper checks it on the
first launch. Without a Developer ID signature and notarization, macOS refuses to open the
app and says that Apple could not check it for malicious software. The supported ways past
this are:

- **System Settings:** try to open the app once, then choose **Open Anyway** in
  **System Settings → Privacy & Security** and confirm. macOS remembers the choice.
- **Control-click → Open:** works up to macOS 14. From macOS 15 on, it no longer bypasses
  Gatekeeper, so use System Settings instead.
- **Terminal:** `xattr -dr com.apple.quarantine /Applications/OxideMerge.app` clears the
  flag. An app built from source, or a package fetched with a tool such as `curl`, never
  gets it.

The release page lists each download's SHA-256 digest, so the downloaded package can be
checked before it is allowed to run (`shasum -a 256 oxidemerge-*-macos-universal.dmg`).

Once the app is opened, macOS may still ask for access to the Desktop, Documents,
Downloads, removable volumes or network volumes the first time a compare reads them. That
is independent of signing; `Info.plist` explains why OxideMerge asks.

## What CI does today

The macOS job in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) builds and
tests `aarch64-apple-darwin` on every run, and opens real windows with
`scripts/gui-smoke.sh` using that build. On pushes, and on manual runs with `packages`, it
then:

1. builds `x86_64-apple-darwin` and joins the two with `lipo`;
2. fails unless `lipo -verify_arch arm64 x86_64` passes;
3. signs the app ad hoc (`codesign --force --deep --sign -`), since Apple silicon refuses
   to run unsigned code, and checks it with `codesign --verify --deep --strict`;
4. runs the packaged binary from the command line, and its Intel half again through
   Rosetta (only a warning when the runner has no Rosetta).

Pull requests build no app.

## What notarization would take

- **An Apple Developer Program membership** (paid, yearly) for the publisher.
- **A Developer ID Application certificate**, exported as a `.p12` with its private key
  and stored as an encrypted CI secret together with its password. CI imports it into a
  temporary keychain.
- **Hardened runtime:** sign with `codesign --options runtime --timestamp`. OxideMerge
  loads no plug-ins and needs no JIT, so it should need no extra entitlements; the `.app`
  must be signed from the inside out rather than with `--deep`.
- **Notarization:** sign the `.dmg` (holding the signed app) with the same certificate
  (`codesign --timestamp`), then submit it with `xcrun notarytool submit --wait`, using an
  App Store Connect API key (key ID, issuer ID and `.p8` file as CI secrets).
- **Stapling:** `xcrun stapler staple` on the `.dmg`, so Gatekeeper accepts it offline.
  The app inside is covered by the same notarization; stapling the app itself as well
  means notarizing it before it goes into the `.dmg`, then building, signing and
  notarizing the `.dmg`.
- **Verification in CI:** `spctl --assess --type execute -vv OxideMerge.app` and
  `spctl --assess --type open --context context:primary-signature -vv` on the `.dmg` must
  report `source=Notarized Developer ID`. This belongs in the macOS job, which would fail
  otherwise: the release job runs on Linux, which has no `spctl`.

Pull requests build no app, and those from forks get no secrets anyway, so only pushes to
`main`, tags and manual runs with `packages` would be notarized.

## Decision

Ad-hoc signing stays for now. Notarization needs a paid membership, a certificate tied to
a person or company, and long-lived secrets in CI, and the project has none of those yet.
The workarounds above are documented in the README, the user guides and every release's
notes.

If notarization is adopted later, the steps above replace step 3 of the CI job, the "not
notarized" passages in the README, the user guides and the release notes go away, and this
note records the change.

## Testing on a clean system

A first launch should be tried on a macOS installation that has never run OxideMerge (a
new user account, or a virtual machine): download the `.dmg` with a browser so it gets the
quarantine flag, check it against the digest on the release page, drag the app to
Applications, open it through **Open Anyway**, and compare two files and two folders in a
protected folder such as Documents. GitHub's hosted runners can't do this, because nothing
they build is quarantined, so it has to be done by hand before a release.
