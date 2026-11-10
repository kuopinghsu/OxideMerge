# macOS signing and notarization

OxideMerge for macOS is a universal `OxideMerge.app` (Apple silicon and Intel) shipped in
a `.dmg`. Releases are signed ad hoc and are not notarized. CI can sign with a Developer ID
and notarize as soon as the project's Apple credentials are added as secrets; this note
explains what users see today, how that pipeline works, and what is still unverified.

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
3. signs the app with [`scripts/macos-sign.sh`](../scripts/macos-sign.sh), from the
   inside out: ad hoc when no signing secret is set, since Apple silicon refuses to run
   unsigned code, or with a Developer ID and the hardened runtime when they all are (see
   below). It checks the result with `codesign --verify --strict` and fails unless the
   signature is of the kind expected;
4. runs the packaged binary from the command line, and its Intel half again through
   Rosetta (only a warning when the runner has no Rosetta);
5. builds the `.dmg` and checks it with `hdiutil verify`; with a Developer ID it is also
   signed, notarized, stapled, and assessed by Gatekeeper (below).

The release job then checks every package as what it claims to be (zip and tar listings,
the AppImage and disk image marks, PDF start and end) with
[`scripts/check-release.sh`](../scripts/check-release.sh), after the packages' trip
through the artifact store, and publishes a `SHA256SUMS` file with them.

Pull requests build no app.

## The Developer ID pipeline

Signing with a Developer ID is switched on by five repository secrets, which reach only
the macOS packaging step:

| Secret | What it holds |
| --- | --- |
| `MACOS_CERTIFICATE_P12` | base64 of the Developer ID Application certificate and key (`.p12`) |
| `MACOS_CERTIFICATE_PASSWORD` | the `.p12` password |
| `MACOS_NOTARY_KEY` | base64 of an App Store Connect API key (`.p8`) |
| `MACOS_NOTARY_KEY_ID` | that key's ID |
| `MACOS_NOTARY_ISSUER` | that key's issuer ID |

With all five, `scripts/macos-sign.sh`:

- imports the certificate into a temporary keychain with a random password, and deletes
  the keychain and the key files in a step that runs even when packaging fails;
- signs the binary, then the app, with `--options runtime --timestamp` (no `--deep`), and
  fails unless the signature names a Developer ID and the hardened runtime;
- signs the `.dmg`, submits it with `xcrun notarytool submit --wait`, staples it, and fails
  unless `spctl` reports `source=Notarized Developer ID` for the `.dmg` and for the app
  inside it.

With none of them (forks, pull requests, and this repository today) the app is signed ad
hoc: the community build path, which needs no credentials. Setting some but not all of them
fails the job, so a broken setup never publishes an ad hoc build by mistake. The script
never traces its commands, so no secret is echoed; GitHub also masks secret values in logs.

**Not yet verified:** the Developer ID path has not run, because the project has no
certificate. Only the ad hoc path has been run, on macOS 26, through `setup`, `app`, `dmg`
and `cleanup`. In particular it is not known yet whether the hardened runtime needs any
entitlement for OxideMerge.

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

Releases stay ad hoc for now. Notarization needs a paid membership, a certificate tied to
a person or company, and long-lived secrets in CI, and the project has none of those yet.
The workarounds above are documented in the README, the user guides and every release's
notes.

Adopting it means adding the five secrets, running a manual CI run with `packages` to see
the Developer ID path pass, and then removing the "not notarized" passages from the
README, the user guides and the release notes in `ci.yml`, with this note recording the
change.

## Testing on a clean system

A first launch should be tried on a macOS installation that has never run OxideMerge (a
new user account, or a virtual machine): download the `.dmg` with a browser so it gets the
quarantine flag, check it against the digest on the release page, drag the app to
Applications, open it through **Open Anyway**, and compare two files and two folders in a
protected folder such as Documents. Then remove it: quit OxideMerge, delete
`/Applications/OxideMerge.app`, and, to remove its settings and recovery snapshots too,
`~/Library/Application Support/OxideMerge`.

`scripts/macos-install-check.sh <dmg> [ad-hoc|developer-id]` runs what can be scripted, and
the macOS job runs it on every DMG it builds: it verifies the image, copies the app into an
Applications folder of its own with the quarantine flag a browser sets, checks Gatekeeper's
verdict (`spctl --assess` must reject an ad hoc app, which is why the README asks for Open
Anyway, and accept a Developer ID app as notarized), clears the flag with the README's
`xattr -dr` command, runs the program on the sample files with settings in a folder of its
own, and removes it, checking that nothing is left. On a Mac where Gatekeeper is turned
off (`spctl --status` says "assessments disabled") every app is accepted, so the script
prints a warning instead of judging the verdict. That was the case on the Mac it was
written on, so the rejection has not been seen yet; the macOS runner, where Gatekeeper is on,
is the first place that checks it.

What it cannot do: click through the first-launch dialog and Open Anyway, or start from a
system that has never run OxideMerge, so the manual test above is still needed before a
release; it has not been recorded for any release yet.
