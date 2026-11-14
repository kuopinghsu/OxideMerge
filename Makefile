# OxideMerge build helpers. Requires GNU Make and a Rust toolchain (cargo).
# Run `make help` for the list of targets.

CARGO ?= cargo
BIN   := oxidemerge
ARGS  ?=

# `cargo pkgid` prints ".../OxideMerge#oxidemerge@0.9.3" (or "...#0.9.3").
# Make before 4.3 (macOS ships 3.81) treats a bare `#` inside a function call as a comment.
HASH    := \#
VERSION := $(lastword $(subst @, ,$(subst $(HASH), ,$(shell $(CARGO) pkgid))))

ifeq ($(OS),Windows_NT)
  EXE      := .exe
  PLATFORM := windows
  # AMD64 or ARM64, named as the CI packages name them; a 32-bit make on 64-bit Windows
  # finds the machine in PROCESSOR_ARCHITEW6432.
  MACHINE  := $(subst AMD64,x64,$(subst ARM64,arm64,$(or $(PROCESSOR_ARCHITEW6432),$(PROCESSOR_ARCHITECTURE))))
  # Where cargo puts its home: the user profile, whatever HOME a Unix-like shell sets.
  USER_HOME := $(or $(USERPROFILE),$(HOME))
else
  EXE      :=
  PLATFORM := $(shell uname -s | tr A-Z a-z)
  MACHINE  := $(shell uname -m)
  USER_HOME := $(HOME)
endif

TARGET_DIR  := $(if $(CARGO_TARGET_DIR),$(subst \,/,$(CARGO_TARGET_DIR)),target)
RELEASE_BIN := $(TARGET_DIR)/release/$(BIN)$(EXE)
DIST_NAME   := $(BIN)-$(VERSION)-$(PLATFORM)-$(MACHINE)
DIST_DIR    := dist/$(DIST_NAME)

.DEFAULT_GOAL := all

.PHONY: all help build release run run-release demo demo-text demo-3way demo-3way-merge demo-3way-folder demo-conflict \
        demo-web demo-web-text demo-web-headings demo-web-tables \
        test gui-smoke docker-test check check-platforms lint fmt fmt-check ci install uninstall shell-register shell-unregister \
        languages dist appimage user-guide user-guide-docs clean

help:
	@echo 'OxideMerge $(VERSION) - make targets'
	@echo '  all               Release build (default, same as release)'
	@echo '  release           Optimized build in $(RELEASE_BIN)'
	@echo '  build             Debug build'
	@echo '  run               Run the debug build, pass arguments with ARGS="left right"'
	@echo '  run-release       Run the release build with ARGS'
	@echo '  demo              Folder compare of samples/left and samples/right'
	@echo '  demo-text         2-way text compare of the sample main.rs files'
	@echo '  demo-3way         3-way text compare of samples/three'
	@echo '  demo-3way-merge   3-way merge of the sample settings files, with conflicts'
	@echo '  demo-3way-folder  3-way folder compare of samples/three/folders'
	@echo '  demo-conflict     Resolve the sample Git conflict file'
	@echo '  demo-web          Web page compare of the sample release notes pages'
	@echo '  demo-web-text     Web page compare of complex text (try the Formatted mode)'
	@echo '  demo-web-headings Web page compare of renamed, moved and re-levelled headings'
	@echo '  demo-web-tables   Web page compare of tables: changed cells, rows and columns'
	@echo '  test              Run the unit, window and command line tests'
	@echo '  gui-smoke         Open real windows with the release build and check what they show'
	@echo '  docker-test       Check Linux in a Docker container, with real test servers'
	@echo '  check             Type-check all targets'
	@echo '  check-platforms   Type-check for Windows, macOS and Linux'
	@echo '  lint              Clippy with warnings treated as errors'
	@echo '  fmt / fmt-check   Format the code / verify formatting'
	@echo '  ci                check + lint + test'
	@echo '  install           cargo install into ~/.cargo/bin'
	@echo '  uninstall         Remove the installed binary'
	@echo '  shell-register    Install, then add the file manager entries (Explorer, Finder, Linux)'
	@echo '  shell-unregister  Remove the file manager entries'
	@echo '  languages         List the available UI languages'
	@echo '  dist              Release build packaged into dist/$(DIST_NAME)'
	@echo '  appimage          Linux AppImage in dist/ (Docker on Windows/macOS)'
	@echo '  user-guide        User guide PDF files (English, Traditional Chinese) in dist/'
	@echo '  user-guide-docs   The latest user guide PDF files in docs/dist, to commit'
	@echo '  clean             Remove build output and dist/'

all: release

build:
	$(CARGO) build

release:
	$(CARGO) build --release
	@echo Built $(RELEASE_BIN)

run:
	$(CARGO) run -- $(ARGS)

run-release:
	$(CARGO) run --release -- $(ARGS)

demo:
	$(CARGO) run -- -r samples/left samples/right

demo-text:
	$(CARGO) run -- samples/left/src/main.rs samples/right/src/main.rs

demo-3way:
	$(CARGO) run -- samples/three/left.txt samples/three/base.txt samples/three/right.txt

demo-3way-merge:
	$(CARGO) run -- samples/three/settings/left.ini samples/three/settings/base.ini samples/three/settings/right.ini

demo-3way-folder:
	$(CARGO) run -- -r samples/three/folders/left samples/three/folders/base samples/three/folders/right

demo-conflict:
	$(CARGO) run -- samples/three/conflict.txt

demo-web:
	$(CARGO) run -- samples/web/left.html samples/web/right.html

demo-web-text demo-web-headings demo-web-tables: demo-web-%:
	$(CARGO) run -- samples/web/formatted/$*/left.html samples/web/formatted/$*/right.html

test:
	$(CARGO) test

gui-smoke: release
	bash scripts/gui-smoke.sh $(RELEASE_BIN)

docker-test:
	bash scripts/docker-test.sh

check:
	$(CARGO) check --all-targets

PLATFORM_TARGETS := x86_64-pc-windows-msvc x86_64-apple-darwin x86_64-unknown-linux-gnu

# Needs the standard libraries: rustup target add $(PLATFORM_TARGETS)
check-platforms:
	$(foreach t,$(PLATFORM_TARGETS),$(CARGO) check --target $(t) &&) echo All platforms OK

lint:
	$(CARGO) clippy --all-targets -- -D warnings

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt --check

ci: check lint test

install:
	$(CARGO) install --path . --locked

uninstall:
	$(CARGO) uninstall $(BIN)

# The context menu points at the binary that registers it, so use the installed one,
# where `cargo install` put it, whatever is first on the PATH.
INSTALLED := $(subst \,/,$(or $(CARGO_INSTALL_ROOT),$(CARGO_HOME),$(USER_HOME)/.cargo))/bin/$(BIN)$(EXE)

shell-register: install
	"$(INSTALLED)" --register-shell

shell-unregister:
	"$(INSTALLED)" --unregister-shell

languages: build
	$(CARGO) run -- --list-languages

dist: release
ifeq ($(OS),Windows_NT)
	powershell -NoProfile -Command "New-Item -ItemType Directory -Force -Path '$(DIST_DIR)' | Out-Null; Copy-Item -Force '$(RELEASE_BIN)', 'README.md', 'LICENSE' '$(DIST_DIR)'; Remove-Item -Force -ErrorAction SilentlyContinue '$(DIST_DIR).zip'; Add-Type -AssemblyName System.IO.Compression.FileSystem; [IO.Compression.ZipFile]::CreateFromDirectory('$(DIST_DIR)', '$(DIST_DIR).zip')"
	@echo Created $(DIST_DIR).zip
else
	mkdir -p $(DIST_DIR)
	cp $(RELEASE_BIN) README.md LICENSE $(DIST_DIR)/
	tar -C dist -czf $(DIST_DIR).tar.gz $(DIST_NAME)
	@echo Created $(DIST_DIR).tar.gz
endif

# Keep the native Linux flow; other hosts build the Linux binary and AppDir in Docker.
ifneq ($(filter windows darwin,$(PLATFORM)),)
APPIMAGE_IMAGE := oxidemerge-appimage

.PHONY: appimage-image
appimage-image:
	docker build -t $(APPIMAGE_IMAGE) -f docker/appimage.Dockerfile docker

appimage: appimage-image
	docker run --rm --init --mount "type=bind,source=$(CURDIR),target=/work" --mount "type=volume,source=oxidemerge-appimage-target-$(MACHINE),target=/target" --mount "type=volume,source=oxidemerge-appimage-registry,target=/root/.cargo/registry" --workdir /work --env "VERSION=$(VERSION)" --env CARGO_BUILD_JOBS $(APPIMAGE_IMAGE)
else ifeq ($(PLATFORM),linux)
appimage: release
	VERSION='$(VERSION)' bash scripts/package-appimage.sh
else
appimage:
	@echo AppImage packages are built on Linux x86_64 or aarch64 >&2
	@exit 1
endif

# Same PDF files as the CI user-guide job; scripts/build-user-guide.sh lists what it needs.
# Windows and macOS have neither its tools nor its fonts as Linux packages them, so there the
# script runs in a Docker container that does; on Linux it runs as it is.
ifneq ($(filter windows darwin,$(PLATFORM)),)
GUIDE_IMAGE := oxidemerge-user-guide
GUIDE_DOCKER := docker run --rm --mount "type=bind,source=$(CURDIR),target=/work" --workdir /work --env "VERSION=$(VERSION)"

.PHONY: user-guide-image
user-guide-image:
	docker build -t $(GUIDE_IMAGE) -f docker/user-guide.Dockerfile docker

user-guide: user-guide-image
	$(GUIDE_DOCKER) $(GUIDE_IMAGE) bash scripts/build-user-guide.sh dist

user-guide-docs: user-guide-image
	$(GUIDE_DOCKER) --env UNVERSIONED=1 $(GUIDE_IMAGE) bash scripts/build-user-guide.sh docs/dist
else
user-guide:
	VERSION='$(VERSION)' bash scripts/build-user-guide.sh dist

# The guides kept in the repository, under names that stay the same from one version to the next.
user-guide-docs:
	UNVERSIONED=1 VERSION='$(VERSION)' bash scripts/build-user-guide.sh docs/dist
endif

clean:
	$(CARGO) clean
ifeq ($(OS),Windows_NT)
	powershell -NoProfile -Command "Remove-Item -Recurse -Force -ErrorAction SilentlyContinue dist"
else
	rm -rf dist
endif
