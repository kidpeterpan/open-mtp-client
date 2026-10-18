VIRTUAL_PHONE ?= $(CURDIR)/.virtual-phone

.PHONY: run run-virtual run-virtual-two build release universal app icon test test-intel test-large test-vendor cli lint fmt coverage

# Line coverage the workspace must keep.
COVERAGE_MIN ?= 75

# `make run` builds optimized: the window redraws a lot, and a debug build of
# it is many times slower (felt most in a folder with thousands of photos).
# `make run PROFILE=` builds a debug binary, which compiles faster.
PROFILE ?= --release

run:
	cargo run $(PROFILE) -p open-mtp-client

run-virtual:
	mkdir -p $(VIRTUAL_PHONE)/DCIM/Camera $(VIRTUAL_PHONE)/Download $(VIRTUAL_PHONE)/Music
	OPEN_MTP_CLIENT_VIRTUAL_DIR=$(VIRTUAL_PHONE) cargo run $(PROFILE) -p open-mtp-client

# Two fake phones plugged in at once, to try switching between them.
run-virtual-two:
	mkdir -p $(VIRTUAL_PHONE)/DCIM/Camera $(VIRTUAL_PHONE)-2/DCIM/Camera $(VIRTUAL_PHONE)-2/Music
	OPEN_MTP_CLIENT_VIRTUAL_DIR=$(VIRTUAL_PHONE):$(VIRTUAL_PHONE)-2 cargo run $(PROFILE) -p open-mtp-client

build:
	cargo build --workspace

release:
	cargo build --workspace --release

VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
APP := dist/open-mtp-client.app
ICONSET := target/open-mtp-client.iconset

# Redraws assets/icon-1024.png (checked in, so this is only needed after
# changing scripts/make_icon.py).
icon:
	python3 scripts/make_icon.py assets/icon-1024.png

# One binary for both kinds of Mac, Apple Silicon and Intel (separate
# downloads for each are easy to mix up, so one app serves both). Each
# half is built for the oldest macOS Info.plist promises; `lipo` joins them,
# and the build fails unless both are really in there.
TARGETS := aarch64-apple-darwin x86_64-apple-darwin
MACOS_MIN := $(shell /usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' packaging/Info.plist)
UNIVERSAL := target/universal/open-mtp-client
UNIVERSAL_CLI := target/universal/open-mtp-client-cli

# The app and its command-line tool, each joined for both kinds of Mac.
universal:
	rustup target add $(TARGETS)
	for target in $(TARGETS); do \
		MACOSX_DEPLOYMENT_TARGET=$(MACOS_MIN) cargo build --release -p open-mtp-client -p open-mtp-client-cli --target $$target || exit 1; \
	done
	mkdir -p $(dir $(UNIVERSAL))
	lipo -create -output $(UNIVERSAL) $(foreach target,$(TARGETS),target/$(target)/release/open-mtp-client)
	lipo -create -output $(UNIVERSAL_CLI) $(foreach target,$(TARGETS),target/$(target)/release/open-mtp-client-cli)
	for arch in arm64 x86_64; do lipo $(UNIVERSAL) -verify_arch $$arch || exit 1; lipo $(UNIVERSAL_CLI) -verify_arch $$arch || exit 1; done

# A double-clickable dist/open-mtp-client.app with its own icon, for both
# kinds of Mac.
app: universal
	rm -rf $(APP) $(ICONSET)
	mkdir -p $(APP)/Contents/MacOS $(APP)/Contents/Resources $(ICONSET)
	for size in 16 32 128 256 512; do \
		sips -z $$size $$size assets/icon-1024.png --out $(ICONSET)/icon_$${size}x$${size}.png >/dev/null; \
		sips -z $$((size * 2)) $$((size * 2)) assets/icon-1024.png --out $(ICONSET)/icon_$${size}x$${size}@2x.png >/dev/null; \
	done
	iconutil -c icns $(ICONSET) -o $(APP)/Contents/Resources/open-mtp-client.icns
	sed 's/@VERSION@/$(VERSION)/g' packaging/Info.plist > $(APP)/Contents/Info.plist
	plutil -lint $(APP)/Contents/Info.plist
	cp $(UNIVERSAL) $(APP)/Contents/MacOS/open-mtp-client
	# The command-line tool goes in too (Help's About says where), so a
	# script needs nothing but the app.
	cp $(UNIVERSAL_CLI) $(APP)/Contents/MacOS/open-mtp-client-cli
	# Ad hoc, as the linker signs a single-architecture build: Apple Silicon
	# runs no unsigned code, and joining the two halves loses their signatures.
	# Nested code is signed before the bundle that holds it.
	codesign --force --sign - $(APP)/Contents/MacOS/open-mtp-client-cli
	codesign --force --sign - $(APP)
	codesign --verify --strict $(APP)
	$(APP)/Contents/MacOS/open-mtp-client-cli --help >/dev/null
	@echo "Built $(APP)"

test:
	cargo test --workspace

# The whole suite as Intel code: on Apple Silicon it runs under Rosetta, so
# the Intel half of the universal app is tested too, not only built.
test-intel:
	rustup target add x86_64-apple-darwin
	cargo test --workspace --target x86_64-apple-darwin

# The two tests that move a whole 4.7 GB file each way (sparse on disk, but
# written for real at the other end), kept out of `make test` to spare the disk
# and the debug-build time; seconds on a release build.
test-large:
	cargo test --release -p open-mtp-client-internal big_files -- --ignored --test-threads=1

# The vendored mtp-rs's own unit tests, which `make test` leaves alone (it is
# outside the workspace) and which hold the checks on what a phone sends it.
# Needs the network the first time (its dev-dependencies).
test-vendor:
	cd vendor/mtp-rs && CARGO_TARGET_DIR=../../target/vendor cargo test --lib

# The command-line tool, for scripts: open-mtp-client-cli --help.
cli:
	cargo build --release -p open-mtp-client-cli

lint:
	cargo fmt --all --check
	cargo clippy --workspace --all-targets -- -D warnings

fmt:
	cargo fmt --all

coverage:
	cargo llvm-cov --workspace --summary-only --fail-under-lines $(COVERAGE_MIN)
