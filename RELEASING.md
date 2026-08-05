# Releasing dbui

A release is one universal (Intel + Apple Silicon) `.app`, signed with a
Developer ID certificate, notarized by Apple, and stapled — so a downloaded copy
opens with no Gatekeeper warning, no right-click, no `xattr -d`.

Releases are cut **from a Mac**, by hand. Notarization needs the Developer ID
certificate in a local keychain, so there is no CI here to run it: the `make`
targets below are the whole pipeline.

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

Both `release-macos` and `publish` run `make check-version` first, which refuses
a `TAG` that disagrees with `Cargo.toml` before anything expensive happens.

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
