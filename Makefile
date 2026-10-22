# OxideMerge build helpers. Requires GNU Make and a Rust toolchain (cargo).
# Run `make help` for the list of targets.

CARGO ?= cargo
BIN   := oxidemerge
ARGS  ?=

# `cargo pkgid` prints ".../OxideMerge#oxidemerge@0.9.0" (or "...#0.9.0").
# Make before 4.3 (macOS ships 3.81) treats a bare `#` inside a function call as a comment.
HASH    := \#
VERSION := $(lastword $(subst @, ,$(subst $(HASH), ,$(shell $(CARGO) pkgid))))

ifeq ($(OS),Windows_NT)
  EXE      := .exe
  PLATFORM := windows
else
  EXE      :=
  PLATFORM := $(shell uname -s | tr A-Z a-z)
endif

TARGET_DIR  := $(if $(CARGO_TARGET_DIR),$(subst \,/,$(CARGO_TARGET_DIR)),target)
RELEASE_BIN := $(TARGET_DIR)/release/$(BIN)$(EXE)
DIST_NAME   := $(BIN)-$(VERSION)-$(PLATFORM)
DIST_DIR    := dist/$(DIST_NAME)

.DEFAULT_GOAL := all

.PHONY: all help build release run run-release demo demo-text demo-3way demo-conflict \
        test check check-platforms lint fmt fmt-check ci install uninstall shell-register shell-unregister \
        languages dist clean

help:
	@echo OxideMerge $(VERSION) - make targets
	@echo   all               Release build (default, same as release)
	@echo   release           Optimized build in $(RELEASE_BIN)
	@echo   build             Debug build
	@echo   run               Run the debug build, pass arguments with ARGS="left right"
	@echo   run-release       Run the release build with ARGS
	@echo   demo              Folder compare of samples/left and samples/right
	@echo   demo-text         2-way text compare of the sample main.rs files
	@echo   demo-3way         3-way text compare of samples/three
	@echo   demo-conflict     Resolve the sample Git conflict file
	@echo   test              Run the unit tests
	@echo   check             Type-check all targets
	@echo   check-platforms   Type-check for Windows, macOS and Linux
	@echo   lint              Clippy with warnings treated as errors
	@echo   fmt / fmt-check   Format the code / verify formatting
	@echo   ci                check + lint + test
	@echo   install           cargo install into ~/.cargo/bin
	@echo   uninstall         Remove the installed binary
	@echo   shell-register    Install, then add the Explorer context menu (Windows)
	@echo   shell-unregister  Remove the Explorer context menu (Windows)
	@echo   languages         List the available UI languages
	@echo   dist              Release build packaged into dist/$(DIST_NAME)
	@echo   clean             Remove build output and dist/

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

demo-conflict:
	$(CARGO) run -- samples/three/conflict.txt

test:
	$(CARGO) test

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

# The context menu points at the binary that registers it, so use the installed one.
shell-register: install
	$(BIN)$(EXE) --register-shell

shell-unregister:
	$(BIN)$(EXE) --unregister-shell

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

clean:
	$(CARGO) clean
ifeq ($(OS),Windows_NT)
	powershell -NoProfile -Command "Remove-Item -Recurse -Force -ErrorAction SilentlyContinue dist"
else
	rm -rf dist
endif
