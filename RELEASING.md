# Releasing dbui

A release is one universal (Intel + Apple Silicon) `.app`, signed with the
project's own self-signed certificate. It is **not** notarized — there is no
Apple Developer ID behind it — so a copy downloaded in a browser is stopped by
Gatekeeper once, until the user clicks *Open Anyway* (the README walks them
through it). Updates installed from inside the app are not quarantined and open
without asking.

Releases are cut **from a Mac**, by hand, because the signing key lives in that
Mac's keychain. The `make` targets below are the whole pipeline.

## What gets published

| Asset | For |
| --- | --- |
| `dbui-<version>-universal.dmg` | what people download and drag to Applications |
| `dbui-<version>-universal.zip` | what the in-app updater downloads |
| `SHA256SUMS` | checked by the updater before it expands anything |

## One-time setup

### The release certificate

```sh
make signing-cert
```

This creates a self-signed code-signing certificate named **dbui Release
Signing**, imports it into your login keychain, and trusts it for code signing.
It asks for a password for the backup, then for your macOS password (the trust
change is a system prompt). Confirm it landed:

```sh
security find-identity -v -p codesigning | grep "dbui Release Signing"
```

The `Makefile` picks it up by name. With no certificate installed, builds fall
back to an ad-hoc signature — fine for running locally, and refused by both
`release-macos` and `publish`.

> **Back up `~/dbui-signing/dbui-signing.p12` and its password** somewhere
> that is not this Mac — a password manager is ideal. Every copy of dbui in the
> wild only accepts updates signed by *this* certificate. Lose the private key
> and none of them can auto-update again; every user has to reinstall by hand.

### A new Mac

Do **not** run `make signing-cert` again — a new certificate is a new identity.
Restore the old one instead:

```sh
security import dbui-signing.p12 -T /usr/bin/codesign
security add-trusted-cert -r trustRoot -p codeSign \
  -k ~/Library/Keychains/login.keychain-db dbui-signing.cer
```

## Before you cut one

```sh
make preflight                # formatting, the whole test suite, a clippy report
```

`preflight` gates on formatting and tests. The suite it runs ends with
`a_whole_session_from_connect_to_commit`, which is the check worth knowing
about: it starts a real window on a real SQLite file and drives one session
through it — connect, read the catalog, open a table, sort it, drag a column
somewhere else, run a query and sort the page in hand, open the templates
palette, reorder the tabs and close a run of them, then edit a cell and commit
it. It finishes by opening the database file on a *second* connection and
asking whether the edit is actually in there, because the app's own answer is
the thing being tested.

Formatting, clippy (`-D warnings`) and the tests are all hard gates. Keep them
that way — a lint left to rot is a lint everyone learns to scroll past.

`preflight` covers SQLite, which is linked in and needs nothing running. The
Postgres and MySQL adapters are only exercised with servers up, and they are
where the engine-specific decoding lives — arrays, `DATETIME` versus
`TIMESTAMP`, non-finite floats. Before a release, run those too:

```sh
docker compose up -d          # postgres on 55432, mysql on 53306
DBUI_LIVE_TESTS=1 cargo test  # the same suite, plus ~46 tests against both
docker compose down
```

Without `DBUI_LIVE_TESTS` those tests print why they did nothing and pass, so a
green `make preflight` on its own says nothing about either engine.

## Releasing from your Mac

```sh
# 1. bump [workspace.package] version in Cargo.toml, commit, push
make preflight                # formatting + tests, including the full session
make release-macos            # build, sign, package
make smoke                    # the bundled app actually starts
make publish TAG=v0.1.0       # create the GitHub release from build/
```

`preflight` proves the UI works in-process; it cannot prove that *this bundle*
starts. `make smoke` execs the binary inside `build/dbui.app` and fails if it
is not still running a few seconds later — a resource left out of the bundle, a
signature the hardened runtime rejects, or a broken universal slice all look
fine until something opens the thing Apple hands a user.

Builds both slices, `lipo`s them together, bundles, signs the app, builds and
packs the `.dmg`, and writes the `.zip` and `SHA256SUMS`. The `.dmg` itself
is deliberately left unsigned: Gatekeeper refuses to mount a disk image signed
with a certificate Apple has not notarized, so a signed one would never open.
The app inside it is signed, and `make verify` checks that.

It finishes with `make verify`:

```
build/dbui.app: valid on disk
build/dbui.app: satisfies its Designated Requirement
requirement: identifier "com.gzenit.dbui" and certificate root = H"…"
archs: x86_64 arm64
```

The `requirement:` line is the one to look for: it has to name a certificate.
`cdhash H"…"` there means the build was signed ad-hoc and no installed copy
would accept it.

`make publish` then uploads the three artifacts and creates the release. It
builds nothing — it only uploads what `release-macos` left behind, and it
re-checks the signature against the release certificate first, so it cannot
publish an ad-hoc build by accident.

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

1. pass `codesign --verify --deep --strict`, and
2. satisfy the running copy's **designated requirement** — the bundle
   identifier plus the hash of the certificate that signed it. Only a bundle
   signed with the same private key passes.

There is no Gatekeeper (`spctl`) check: an unnotarized build would always fail
it. The certificate check is what stands in for it, which is why the key has to
be kept safe *and* kept at all.

Builds up to 0.2.8 were signed with a Developer ID the project no longer has.
Their updater checks for that team, so it refuses every release from here on:
those users have to download one release by hand, after which updates work
again. Say so in the release notes of the first release signed this way.

The updater is inert unless the app is running from a `.app` bundle, so a
`cargo run` build never tries to replace `target/debug/`.
