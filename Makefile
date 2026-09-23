# dbui — build, bundle, sign, release.
#
# The short version:
#
#   make run                 debug build, straight to a window
#   make test                the whole workspace
#   make signing-cert        one-time: create the release signing certificate
#   make bundle              build/dbui.app (universal, signed if the cert exists)
#   make release-macos       bundle + .dmg + .zip + SHA256SUMS
#
# `release-macos` is the one that produces GitHub release assets. It needs the
# self-signed release certificate from `signing-cert`; see RELEASING.md.

SHELL := /bin/bash

# One source of truth for the version: the workspace manifest. A release tag
# that disagrees with it is caught by `check-version`.
VERSION := $(shell awk '/^\[workspace.package\]/{f=1} f&&/^version/{gsub(/[",]/,"",$$3); print $$3; exit}' Cargo.toml)

BUILD    := build
APP      := $(BUILD)/dbui.app
APP_BIN  := $(APP)/Contents/MacOS
APP_RES  := $(APP)/Contents/Resources
BIN      := $(BUILD)/dbui
DMG      := $(BUILD)/dbui-$(VERSION)-universal.dmg
ZIP      := $(BUILD)/dbui-$(VERSION)-universal.zip

# Both slices of the universal binary. dbui has no per-architecture payload --
# the fonts are `include_bytes!`'d into the executable -- so one fat binary
# covers Intel and Apple Silicon, and there is only ever one download.
TARGETS  := aarch64-apple-darwin x86_64-apple-darwin
SLICES   := $(foreach t,$(TARGETS),target/$(t)/release/dbui)

# Signing. Releases are signed with a self-signed certificate rather than an
# Apple Developer ID, so they are *not* notarized: a browser download is
# Gatekeeper-blocked until the user picks "Open Anyway" once (see README.md).
# What the certificate buys is a stable identity -- the in-app updater only
# installs a bundle signed by the same certificate as the running copy, and the
# keychain keeps its "Always Allow" across releases.
#
# CODESIGN_ID is the certificate's SHA-1, auto-selected from the keychain by
# name; with none installed it falls back to an ad-hoc signature, which runs
# locally but can never be published (see `publish`).
SIGN_CERT_NAME := dbui Release Signing
# Where `signing-cert` leaves the password-protected backup of the private key.
SIGNING_DIR    ?= $(HOME)/dbui-signing
# Must match CFBundleIdentifier in packaging/Info.plist.in: signing debug builds
# under the same identifier gives them the same code identity as the shipped
# app, so both are the same "application" as far as the keychain is concerned.
BUNDLE_ID      := com.gzenit.dbui
CODESIGN_ID    ?= $(shell security find-identity -v -p codesigning 2>/dev/null \
                    | awk '/"$(SIGN_CERT_NAME)"/{print $$2; exit}')
# What a release bundle has to satisfy: our identifier, signed by our
# certificate. This is the same requirement the updater checks on the far end.
RELEASE_REQ    := identifier "$(BUNDLE_ID)" and certificate root = H"$(CODESIGN_ID)"

.PHONY: all run sign-dev test preflight smoke fmt clippy check-version icon \
        signing-cert universal bundle sign \
        dmg zip-app checksums release-macos publish \
        verify clean

all: bundle

# -- development ----------------------------------------------------------

run: sign-dev
	@target/debug/dbui

# Cargo leaves the debug binary ad-hoc (linker) signed, and an ad-hoc
# signature's designated requirement is the binary's own hash -- which changes
# on every rebuild. The keychain matches that requirement when deciding whether
# an app may read a secret, so each rebuild looks like a different application
# and re-prompts for every saved connection password. Re-signing with the
# release certificate (`make signing-cert`) under a fixed identifier makes the
# requirement identity-based, and one "Always Allow" then holds across rebuilds.
#
# The first run after switching still prompts once per existing secret, because
# those ACLs were granted to the old ad-hoc hashes.
sign-dev:
	@cargo build -p dbui
	@if [ -n "$(CODESIGN_ID)" ]; then \
	    echo "  SIGN  target/debug/dbui ($(BUNDLE_ID))"; \
	    codesign --force --identifier $(BUNDLE_ID) \
	        --sign "$(CODESIGN_ID)" target/debug/dbui; \
	else \
	    echo "  SIGN  skipped (no '$(SIGN_CERT_NAME)' cert -- expect keychain prompts every rebuild)"; \
	fi

test:
	@cargo test --workspace

# Everything that has to be true before a release is cut, in one command.
#
# The e2e suite this runs includes `a_whole_session_from_connect_to_commit`,
# which drives a real window against a real SQLite file -- connect, browse,
# sort, rearrange, query, edit, commit -- and then asks the database, on a
# connection of its own, whether the commit is really there.
#
# All three are hard gates. Keep them that way: a lint left to rot is a lint
# everyone learns to scroll past, and the backlog this target started life
# reporting rather than failing on took one sitting to clear.
preflight:
	@echo "  FMT   --check"
	@cargo fmt --all -- --check
	@echo "  CLIPPY -D warnings"
	@cargo clippy --workspace --all-targets -- -D warnings
	@echo "  TEST  workspace"
	@cargo test --workspace
	@echo "  ->    preflight clean -- 'make release-macos', then 'make smoke'"

# Launch the built app and make sure it is still up a moment later.
#
# `preflight` proves the UI works in-process; it cannot prove that *this
# bundle* starts. A resource left out of the bundle, a signature the hardened
# runtime rejects, a broken universal slice -- none of those show up until
# something actually execs the binary Apple will hand a user.
#
# Point it somewhere else to smoke a different build:
#   make smoke SMOKE_BIN=target/debug/dbui
SMOKE_BIN ?= $(APP_BIN)/dbui
SMOKE_SECONDS ?= 4
smoke:
	@test -x "$(SMOKE_BIN)" || \
	    (echo "ERROR: no $(SMOKE_BIN) -- run 'make bundle' first"; exit 1)
	@echo "  SMOKE $(SMOKE_BIN)"
	@"$(SMOKE_BIN)" & pid=$$!; \
	  sleep $(SMOKE_SECONDS); \
	  if kill -0 $$pid 2>/dev/null; then \
	      echo "        still running after $(SMOKE_SECONDS)s"; \
	      kill $$pid 2>/dev/null; wait $$pid 2>/dev/null || true; \
	      echo "  ->    ok"; \
	  else \
	      wait $$pid; status=$$?; \
	      echo "ERROR: it exited on its own (status $$status)"; exit 1; \
	  fi

fmt:
	@cargo fmt --all

clippy:
	@cargo clippy --workspace --all-targets -- -D warnings

# Refuse to release under a tag that disagrees with Cargo.toml. Called by the
# release workflow, where TAG is the pushed tag (`v0.1.0`).
check-version:
	@if [ -n "$(TAG)" ] && [ "$(TAG)" != "v$(VERSION)" ]; then \
	    echo "ERROR: tag $(TAG) does not match Cargo.toml version $(VERSION)"; exit 1; \
	fi
	@echo "  VERSION $(VERSION)"

# -- icon -----------------------------------------------------------------

# Regenerate packaging/dbui.icns from the master PNG. Both are committed, so a
# release build never needs Pillow -- this only runs when the icon changes.
icon:
	@echo "  ICON  packaging/dbui.icns"
	@python3 packaging/icon.py packaging/icon-master.png
	@rm -rf $(BUILD)/dbui.iconset && mkdir -p $(BUILD)/dbui.iconset
	@for sz in 16 32 128 256 512; do \
	    sips -z $$sz $$sz packaging/icon-master.png \
	        --out $(BUILD)/dbui.iconset/icon_$${sz}x$${sz}.png >/dev/null; \
	    d=$$((sz*2)); sips -z $$d $$d packaging/icon-master.png \
	        --out $(BUILD)/dbui.iconset/icon_$${sz}x$${sz}@2x.png >/dev/null; \
	done
	@iconutil -c icns $(BUILD)/dbui.iconset -o packaging/dbui.icns
	@rm -rf $(BUILD)/dbui.iconset

# -- signing certificate --------------------------------------------------

# Create the self-signed release certificate, import it into the login
# keychain, and trust it for code signing. Run once, on the release machine.
#
# It asks for two things: a password for the backup .p12 it writes to
# $(SIGNING_DIR), and your macOS password (the trust change is a system prompt).
#
# BACK UP THE .p12. The updater in every shipped copy only accepts bundles
# signed by this exact certificate; lose the private key and those copies can
# never auto-update again -- their users have to reinstall by hand.
signing-cert:
	@if [ -n "$(CODESIGN_ID)" ]; then \
	    echo "ERROR: '$(SIGN_CERT_NAME)' already exists ($(CODESIGN_ID)) -- a second"; \
	    echo "       one would break updates for every copy signed with the first."; \
	    exit 1; \
	fi
	@test ! -e "$(SIGNING_DIR)/dbui-signing.p12" || \
	    (echo "ERROR: $(SIGNING_DIR)/dbui-signing.p12 exists -- import it instead of"; \
	     echo "       making a new one (RELEASING.md, 'A new Mac')."; \
	     exit 1)
	@mkdir -p "$(SIGNING_DIR)" && chmod 700 "$(SIGNING_DIR)"
	@work=$$(mktemp -d) && trap 'rm -rf "$$work"' EXIT && \
	printf '%s\n' '[req]' 'distinguished_name=dn' 'prompt=no' \
	    '[dn]' 'CN=$(SIGN_CERT_NAME)' \
	    '[ext]' 'basicConstraints=critical,CA:false' \
	    'keyUsage=critical,digitalSignature' 'extendedKeyUsage=critical,codeSigning' \
	    > "$$work/cfg" && \
	openssl req -x509 -newkey rsa:3072 -nodes -days 7300 -config "$$work/cfg" \
	    -extensions ext -keyout "$$work/key.pem" -out "$(SIGNING_DIR)/dbui-signing.cer" \
	    2>/dev/null && \
	read -rsp "  Password for the backup .p12: " pw && echo && \
	{ test -n "$$pw" || { echo "ERROR: empty password"; exit 1; }; } && \
	PW="$$pw" openssl pkcs12 -export -inkey "$$work/key.pem" \
	    -in "$(SIGNING_DIR)/dbui-signing.cer" -name "$(SIGN_CERT_NAME)" \
	    -out "$(SIGNING_DIR)/dbui-signing.p12" -passout env:PW && \
	chmod 600 "$(SIGNING_DIR)/dbui-signing.p12" && \
	security import "$(SIGNING_DIR)/dbui-signing.p12" -P "$$pw" -T /usr/bin/codesign && \
	echo "  TRUST (macOS will ask for your password)" && \
	security add-trusted-cert -r trustRoot -p codeSign \
	    -k ~/Library/Keychains/login.keychain-db "$(SIGNING_DIR)/dbui-signing.cer"
	@security find-identity -v -p codesigning | grep "$(SIGN_CERT_NAME)" || \
	    (echo "ERROR: the certificate is not a valid signing identity"; exit 1)
	@echo "  ->    BACK UP $(SIGNING_DIR)/dbui-signing.p12 and its password somewhere safe"

# -- build ----------------------------------------------------------------

# One release build per architecture, then `lipo` them into a fat binary.
# Cross-compiling to the other slice needs no extra toolchain on macOS: clang
# takes -arch for either, and rustup ships both std libraries.
universal:
	@echo "  CARGO dbui $(VERSION) (release, universal)"
	@for t in $(TARGETS); do \
	    echo "  ->    $$t"; \
	    cargo build --release -p dbui --target $$t || exit 1; \
	done
	@mkdir -p $(BUILD)
	@lipo -create -output $(BIN) $(SLICES)
	@echo "  LIPO  $$(lipo -archs $(BIN))"

# -- bundle ---------------------------------------------------------------

# Assemble dbui.app. There is no vendored payload to place: everything the app
# needs at runtime is either linked in or lives in the user's home directory.
bundle: universal
	@echo "  BUNDLE $(APP) ($(VERSION))"
	@rm -rf $(APP)
	@mkdir -p $(APP_BIN) $(APP_RES)
	@cp $(BIN) $(APP_BIN)/dbui
	@cp packaging/dbui.icns $(APP_RES)/dbui.icns
	@sed 's/__VERSION__/$(VERSION)/g' packaging/Info.plist.in > $(APP)/Contents/Info.plist
	@printf 'APPL????' > $(APP)/Contents/PkgInfo
	@touch $(APP)
	@$(MAKE) --no-print-directory sign

# Sign inside-out, with the hardened runtime. No secure timestamp: Apple's
# timestamp service is for Developer ID signatures, and nothing here checks one.
sign:
	@if [ -n "$(CODESIGN_ID)" ]; then \
	    echo "  SIGN  $(SIGN_CERT_NAME) (hardened runtime)"; \
	    codesign --force --options runtime --identifier $(BUNDLE_ID) \
	        --sign "$(CODESIGN_ID)" $(APP_BIN)/dbui; \
	    codesign --force --options runtime --identifier $(BUNDLE_ID) \
	        --sign "$(CODESIGN_ID)" $(APP); \
	else \
	    echo "  SIGN  ad-hoc (no '$(SIGN_CERT_NAME)' cert -- not publishable)"; \
	    codesign --force --deep --sign - $(APP) >/dev/null 2>&1 || true; \
	fi

# -- package --------------------------------------------------------------

# A drag-to-Applications disk image.
dmg:
	@echo "  DMG   $(DMG)"
	@rm -rf $(BUILD)/dmgroot $(DMG)
	@mkdir -p $(BUILD)/dmgroot
	@cp -R $(APP) $(BUILD)/dmgroot/
	@ln -s /Applications $(BUILD)/dmgroot/Applications
	@# `hdiutil create` fails with "Resource busy" if anything still has the
	@# freshly-copied bundle open -- Spotlight indexing it is enough. Retry
	@# rather than fail a release build on a race with the indexer.
	@for attempt in 1 2 3 4 5; do \
	    if hdiutil create -volname "dbui" -srcfolder $(BUILD)/dmgroot \
	           -ov -format UDZO $(DMG) >/dev/null 2>$(BUILD)/hdiutil.err; then \
	        break; \
	    fi; \
	    if [ $$attempt = 5 ]; then \
	        echo "ERROR: hdiutil failed after 5 attempts:"; \
	        cat $(BUILD)/hdiutil.err; exit 1; \
	    fi; \
	    echo "        hdiutil busy, retrying ($$attempt/5)"; sleep 3; \
	done
	@rm -rf $(BUILD)/dmgroot $(BUILD)/hdiutil.err
	@# Sign the image too, so a tampered download fails `codesign --verify`.
	@if [ -n "$(CODESIGN_ID)" ]; then \
	    codesign --force --sign "$(CODESIGN_ID)" $(DMG); \
	fi
	@echo "  ->    $(DMG)"

# Zip whatever dbui.app is sitting in build/, without touching it. `ditto`
# rather than `zip` because it preserves the bundle's symlinks and extended
# attributes, which the code signature depends on.
#
# This is the asset the in-app updater downloads: a .zip can be expanded and
# swapped in place, where a .dmg would have to be mounted first.
zip-app:
	@echo "  ZIP   $(ZIP)"
	@rm -f $(ZIP)
	@cd $(BUILD) && /usr/bin/ditto -c -k --keepParent dbui.app $(notdir $(ZIP))
	@echo "  ->    $(ZIP)"

# Full macOS release: build + sign the app, then the .dmg people download and
# the .zip the updater downloads. Refuses to start without the release
# certificate -- an ad-hoc build is one no installed copy would accept.
release-macos: check-version
	@test -n "$(CODESIGN_ID)" || \
	    (echo "ERROR: no '$(SIGN_CERT_NAME)' certificate -- see 'make signing-cert'"; exit 1)
	@$(MAKE) --no-print-directory bundle
	@$(MAKE) --no-print-directory dmg
	@$(MAKE) --no-print-directory zip-app
	@$(MAKE) --no-print-directory checksums
	@$(MAKE) --no-print-directory verify
	@echo "  DONE  $(DMG) + $(ZIP) (signed, not notarized, universal)"

# The updater checks the download against this before it installs anything, so
# a corrupted download is caught before it is ever expanded.
checksums:
	@echo "  SUMS  $(BUILD)/SHA256SUMS"
	@cd $(BUILD) && shasum -a 256 $(notdir $(DMG)) $(notdir $(ZIP)) > SHA256SUMS
	@sed 's/^/        /' $(BUILD)/SHA256SUMS

# Publish the artifacts already sitting in build/ as a GitHub release, from
# here rather than from a runner. `release-macos` has to have run first --
# this uploads, it does not build, so it cannot publish an unsigned build by
# accident.
#
#   make publish TAG=v0.1.0
publish:
	@test -n "$(TAG)" || (echo "ERROR: pass TAG, e.g. make publish TAG=v$(VERSION)"; exit 1)
	@$(MAKE) --no-print-directory check-version TAG=$(TAG)
	@test -f $(DMG) && test -f $(ZIP) && test -f $(BUILD)/SHA256SUMS || \
	    (echo "ERROR: no release artifacts -- run 'make release-macos' first"; exit 1)
	@# Refuse to publish a build the updater would reject. Catching it here is
	@# the difference between a bad release and no release. `-R=<text>` is
	@# codesign's inline form -- the one `=` is what marks it as text; a second
	@# one is a syntax error, which failed this check for every build.
	@{ test -n "$(CODESIGN_ID)" && \
	    codesign --verify --deep --strict -R='$(RELEASE_REQ)' $(APP) 2>/dev/null; } || \
	    (echo "ERROR: $(APP) is not signed with '$(SIGN_CERT_NAME)' -- run 'make release-macos'"; exit 1)
	@echo "  PUBLISH $(TAG)"
	@gh release create $(TAG) $(DMG) $(ZIP) $(BUILD)/SHA256SUMS \
	    --title "dbui $(TAG)" --generate-notes
	@echo "  ->    $$(gh release view $(TAG) --json url -q .url)"

# What the updater in an installed copy will conclude about the build: a sound
# signature, and a designated requirement naming our certificate. (`spctl`
# would say "rejected" -- expected, since nothing here is notarized.)
verify:
	@echo "  VERIFY $(APP)"
	@codesign --verify --deep --strict --verbose=2 $(APP) 2>&1 | sed 's/^/        /'
	@codesign -d -r- $(APP) 2>&1 | sed -n 's/^.*designated => /        requirement: /p'
	@lipo -archs $(APP_BIN)/dbui | sed 's/^/        archs: /'
	@if [ -f $(DMG) ]; then \
	    echo "  VERIFY $(DMG)"; \
	    codesign --verify --verbose=2 $(DMG) 2>&1 | sed 's/^/        /'; \
	fi

clean:
	@rm -rf $(BUILD)
	@cargo clean
