# dbui — build, bundle, sign, notarize, release.
#
# The short version:
#
#   make run                 debug build, straight to a window
#   make test                the whole workspace
#   make bundle              build/dbui.app (universal, signed if a cert exists)
#   make release-macos       bundle + notarize + .dmg + .zip, all stapled
#
# `release-macos` is the one that produces GitHub release assets. It needs a
# "Developer ID Application" certificate and notarytool credentials; see
# `notarize` below and RELEASING.md.

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

# Signing + notarization (ZENIT GROUP LLC, same identity as edui). CODESIGN_ID
# auto-selects the "Developer ID Application" identity if one is installed, else
# falls back to an ad-hoc signature -- runnable locally, Gatekeeper-blocked when
# downloaded.
TEAM_ID        := D7HN42D467
NOTARY_PROFILE ?= dbui-notary
CODESIGN_ID    ?= $(shell security find-identity -v -p codesigning 2>/dev/null \
                    | awk -F'"' '/Developer ID Application/{print $$2; exit}')

# notarytool takes either a stored keychain profile (local, what a developer
# sets up once) or an Apple ID + app-specific password (CI, where there is no
# login keychain to store anything in). Passing APPLE_ID switches to the latter.
ifdef APPLE_ID
NOTARY_AUTH := --apple-id "$(APPLE_ID)" --team-id "$(TEAM_ID)" --password "$(APPLE_APP_PASSWORD)"
else
NOTARY_AUTH := --keychain-profile "$(NOTARY_PROFILE)"
endif

.PHONY: all run test fmt clippy check-version icon universal bundle sign \
        notarize dmg notarize-dmg zip-app release-macos verify clean

all: bundle

# -- development ----------------------------------------------------------

run:
	@cargo run -p dbui

test:
	@cargo test --workspace

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

# Sign inside-out. With a Developer ID identity, enable the hardened runtime and
# a secure timestamp -- both are required for notarization; otherwise ad-hoc.
sign:
	@if [ -n "$(CODESIGN_ID)" ]; then \
	    echo "  SIGN  $(CODESIGN_ID) (hardened runtime)"; \
	    codesign --force --options runtime --timestamp \
	        --sign "$(CODESIGN_ID)" $(APP_BIN)/dbui; \
	    codesign --force --options runtime --timestamp \
	        --sign "$(CODESIGN_ID)" $(APP); \
	else \
	    echo "  SIGN  ad-hoc (no Developer ID cert -- downloads will be Gatekeeper-blocked)"; \
	    codesign --force --deep --sign - $(APP) >/dev/null 2>&1 || true; \
	fi

# -- notarize -------------------------------------------------------------

# Submit the Developer ID-signed app to Apple's notary service and staple the
# ticket onto dbui.app, so a downloaded copy opens with no Gatekeeper warning --
# no right-click, no xattr. Requires:
#   1. A "Developer ID Application" cert (Xcode > Settings > Accounts >
#      ZENIT GROUP LLC > Manage Certificates > + > Developer ID Application).
#   2. A stored notarytool credential named $(NOTARY_PROFILE), one-time:
#        xcrun notarytool store-credentials $(NOTARY_PROFILE) \
#          --apple-id <apple-id> --team-id $(TEAM_ID) \
#          --password <app-specific-password>   # appleid.apple.com > Sign-In & Security
#      In CI there is no keychain: pass APPLE_ID + APPLE_APP_PASSWORD instead.
notarize:
	@if [ -z "$(CODESIGN_ID)" ]; then \
	    echo "ERROR: no Developer ID Application cert found -- cannot notarize."; exit 1; \
	fi
	@echo "  NOTARIZE submitting $(APP)"
	@/usr/bin/ditto -c -k --keepParent $(APP) $(BUILD)/notarize.zip
	@xcrun notarytool submit $(BUILD)/notarize.zip $(NOTARY_AUTH) --wait
	@xcrun stapler staple $(APP)
	@rm -f $(BUILD)/notarize.zip
	@echo "  ->    stapled $(APP)"

# Notarize the .dmg itself and staple the ticket to it.
#
# Stapling the app is not enough for a .dmg download: Gatekeeper assesses the
# disk image too, and without its own ticket that check needs the network -- so
# a first open offline, or behind a blocked notary endpoint, warns.
notarize-dmg:
	@test -f $(DMG) || (echo "ERROR: no $(DMG) -- run 'make dmg' first"; exit 1)
	@echo "  NOTARIZE $(DMG)"
	@xcrun notarytool submit $(DMG) $(NOTARY_AUTH) --wait
	@xcrun stapler staple $(DMG)
	@xcrun stapler validate $(DMG)
	@echo "  ->    stapled $(DMG)"

# -- package --------------------------------------------------------------

# A drag-to-Applications disk image. Run after `notarize` so the app inside
# carries its own stapled ticket too.
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
	@# Sign the image itself. Stapling alone puts a ticket on it, but Gatekeeper
	@# also wants a signature to assess -- without one `spctl --assess` reports
	@# "no usable signature" even though the app inside is notarized.
	@if [ -n "$(CODESIGN_ID)" ]; then \
	    codesign --force --timestamp --sign "$(CODESIGN_ID)" $(DMG); \
	fi
	@echo "  ->    $(DMG)"

# Zip whatever dbui.app is sitting in build/, without touching it. `ditto`
# rather than `zip` because it preserves the bundle's symlinks and extended
# attributes -- including the stapled notarization ticket.
#
# This is the asset the in-app updater downloads: a .zip can be expanded and
# swapped in place, where a .dmg would have to be mounted first.
zip-app:
	@echo "  ZIP   $(ZIP)"
	@rm -f $(ZIP)
	@cd $(BUILD) && /usr/bin/ditto -c -k --keepParent dbui.app $(notdir $(ZIP))
	@echo "  ->    $(ZIP)"

# Full notarized macOS release: build + sign + notarize + staple the app, then a
# .dmg that is itself notarized + stapled, then the .zip for the updater. Two
# notary submissions, because the app and the disk image are assessed
# separately -- the result opens offline, with no warnings.
release-macos: check-version bundle
	@$(MAKE) --no-print-directory notarize
	@$(MAKE) --no-print-directory dmg
	@$(MAKE) --no-print-directory notarize-dmg
	@$(MAKE) --no-print-directory zip-app
	@$(MAKE) --no-print-directory verify
	@echo "  DONE  $(DMG) + $(ZIP) (notarized + stapled, universal)"

# What a user's machine will conclude about the build. `spctl` is the same
# assessment Gatekeeper runs on first open, so a pass here means a pass there.
verify:
	@echo "  VERIFY $(APP)"
	@codesign --verify --deep --strict --verbose=2 $(APP) 2>&1 | sed 's/^/        /'
	@spctl --assess --type execute --verbose=4 $(APP) 2>&1 | sed 's/^/        /'
	@lipo -archs $(APP_BIN)/dbui | sed 's/^/        archs: /'
	@# The disk image is assessed separately from the app inside it.
	@if [ -f $(DMG) ]; then \
	    echo "  VERIFY $(DMG)"; \
	    spctl --assess --type open --context context:primary-signature -v $(DMG) 2>&1 \
	        | sed 's/^/        /'; \
	fi

clean:
	@rm -rf $(BUILD)
	@cargo clean
