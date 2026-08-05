# Releasing dbui

A release is one universal (Intel + Apple Silicon) `.app`, signed with a
Developer ID certificate, notarized by Apple, and stapled — so a downloaded copy
opens with no Gatekeeper warning, no right-click, no `xattr -d`.

Releases are currently cut **from a Mac**, because notarization needs the
Developer ID certificate and the repository has no signing secrets yet. The
GitHub workflow runs the same `make` targets and is ready to take over — see
[Releasing from a tag](#releasing-from-a-tag).

## What gets published

| Asset | For |
| --- | --- |
| `dbui-<version>-universal.dmg` | what people download and drag to Applications |
| `dbui-<version>-universal.zip` | what the in-app updater downloads |
| `SHA256SUMS` | checked by the updater before it installs anything |

Both the `.app` and the `.dmg` are notarized. They are assessed separately by
Gatekeeper, so both need their own ticket — stapling only the app leaves a first
open *offline* with a warning.

## One-time setup

### 1. A Developer ID Application certificate

Xcode → Settings → Accounts → ZENIT GROUP LLC → Manage Certificates → **+** →
*Developer ID Application*. Confirm it landed:

```sh
security find-identity -v -p codesigning | grep "Developer ID Application"
```

The `Makefile` picks this up automatically. With no certificate installed it
falls back to an ad-hoc signature — fine for running locally, Gatekeeper-blocked
for anyone who downloads it.

### 2. notarytool credentials

An app-specific password, not your Apple ID password: appleid.apple.com →
Sign-In & Security → App-Specific Passwords.

```sh
xcrun notarytool store-credentials dbui-notary \
  --apple-id <your-apple-id> \
  --team-id D7HN42D467 \
  --password <app-specific-password>
```

This is an Apple ID + team credential, not a per-app one — if you already have a
profile from another project, `make notarize NOTARY_PROFILE=<name>` reuses it.

## Releasing from your Mac

```sh
# 1. bump [workspace.package] version in Cargo.toml, commit, push
make release-macos            # build, sign, notarize, staple, package
make publish TAG=v0.1.0       # create the GitHub release from build/
```

Builds both slices, `lipo`s them together, bundles, signs, notarizes the app,
builds the `.dmg`, notarizes *that*, staples both, and writes the `.zip`. The
notary round-trip is a few minutes each; the whole thing is about ten.

It finishes with `make verify`, which is the check that matters:

```
build/dbui.app: accepted
source=Notarized Developer ID
archs: x86_64 arm64
```

`source=Notarized Developer ID` is the line to look for. `Unnotarized Developer
ID` means the signature is good but the notary step did not run.

`make publish` then uploads the three artifacts and creates the release. It
builds nothing — it only uploads what `release-macos` left behind, and it
re-checks `spctl` first, so it cannot publish an unnotarized build by accident.

## Releasing from a tag

The `release` workflow does all of the above on a macOS runner and creates the
release itself. It is **manual-only** right now: without the signing secrets a
tag-triggered run can do nothing but fail, so the `push:` trigger is commented
out in `.github/workflows/release.yml`.

To switch over: add the four secrets below, uncomment that trigger, and then

```sh
# 1. bump the version in Cargo.toml ([workspace.package] version)
# 2. commit it
git tag v0.2.0
git push origin v0.2.0
```

The tag must match `Cargo.toml`. `make check-version` runs first and fails the
build if it does not, before anything expensive happens. Actions is free for
this repository while it stays public, including the macOS runners.

### Repository secrets

Settings → Secrets and variables → Actions:

| Secret | How to produce it |
| --- | --- |
| `MACOS_CERT_P12` | see below |
| `MACOS_CERT_PASSWORD` | the password you chose when exporting the `.p12` |
| `APPLE_ID` | the Apple ID you stored notarytool credentials for |
| `APPLE_APP_PASSWORD` | the app-specific password from setup step 2 |

To produce `MACOS_CERT_P12`: Keychain Access → My Certificates → right-click
*Developer ID Application: ZENIT GROUP LLC* → Export → `.p12`, set a password,
then:

```sh
base64 -i Certificates.p12 | pbcopy
```

Paste that as the secret value. The workflow decodes it into a throwaway
keychain that dies with the runner.

## Version numbers

`Cargo.toml`'s `[workspace.package] version` is the only source of truth. It
feeds the binary (`CARGO_PKG_VERSION`), `Info.plist`, the asset filenames, and
the updater's "is this newer than me" comparison.

Tags are `v<version>`. The updater parses `MAJOR.MINOR.PATCH` with an optional
`-prerelease`; a tag it cannot parse is read as "no update", so a `nightly` tag
will not be offered to anyone.

## The updater

The app asks `api.github.com/repos/JosueRhea/dbui/releases/latest` on launch. If
that is newer, the status bar offers it; clicking downloads the `.zip`, checks
it against `SHA256SUMS`, and stages it beside the installed app. A second click
verifies and swaps it in.

Before it swaps anything, `install` requires the downloaded bundle to:

1. pass `codesign --verify --deep --strict`,
2. pass `spctl --assess --type execute` — the same check Gatekeeper runs, so it
   must be notarized, and
3. carry the **same Team Identifier** as the copy that is running.

A release that is signed but not notarized will download and then refuse to
install. That is the intended behaviour, and it is why `release-macos` notarizes
rather than leaving it as a manual last step.

The updater is inert unless the app is running from a `.app` bundle, so a
`cargo run` build never tries to replace `target/debug/`.
