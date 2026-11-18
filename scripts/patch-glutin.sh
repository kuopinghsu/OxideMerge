#!/usr/bin/env bash
# Patches glutin, as Cargo has unpacked it, for the AppImage build:
#
#   scripts/patch-glutin.sh
#
# On Mesa older than 21 (RHEL 8.4) with the software renderer, glutin's call to
# glXSwapIntervalEXT reaches a driver function that is not there, and the window crashes as it
# opens. scripts/patches/glutin-<version>-mesa-swap-interval.patch makes glutin use Mesa's own
# glXSwapIntervalMESA there, which reports the failure instead (see docs/development.md).
#
# The patch goes into $CARGO_HOME/registry/src, so Cargo.toml and Cargo.lock stay as committed and
# the build is not marked as changed. That copy is shared by every project built with the same
# CARGO_HOME, so run this only where the registry belongs to the build: CI and the AppImage
# container. Cargo does not rebuild a downloaded crate whose files change, so glutin's build
# output for PROFILE (default release-size) is removed. Running it again does nothing more.

set -euo pipefail

cd "$(dirname "$0")/.."

profile=${PROFILE:-release-size}

# The glutin version the build uses, from the lock file.
version=$(awk '$0 == "name = \"glutin\"" { getline; gsub(/"/, "", $3); print $3; exit }' Cargo.lock)
if [ -z "$version" ]; then
    echo "${GITHUB_ACTIONS:+::error::}glutin is not in Cargo.lock" >&2
    exit 1
fi
patch_file=$PWD/scripts/patches/glutin-$version-mesa-swap-interval.patch
if [ ! -f "$patch_file" ]; then
    echo "${GITHUB_ACTIONS:+::error::}No patch for glutin $version: make" \
        "scripts/patches/glutin-$version-mesa-swap-interval.patch (see docs/development.md)" >&2
    exit 1
fi

cargo fetch --locked --quiet
shopt -s nullglob
dirs=("${CARGO_HOME:-$HOME/.cargo}"/registry/src/*/glutin-"$version")
if [ ${#dirs[@]} -eq 0 ]; then
    echo "${GITHUB_ACTIONS:+::error::}glutin $version was not unpacked by cargo fetch" >&2
    exit 1
fi

for dir in "${dirs[@]}"; do
    # git apply works on files outside a repository; GIT_DIR keeps it from finding one above.
    if GIT_DIR=/nonexistent git -C "$dir" apply --reverse --check "$patch_file" 2>/dev/null; then
        echo "glutin $version in $dir is already patched"
    elif GIT_DIR=/nonexistent git -C "$dir" apply "$patch_file"; then
        echo "Patched glutin $version in $dir"
    else
        echo "${GITHUB_ACTIONS:+::error::}${patch_file##*/} does not apply to $dir" >&2
        exit 1
    fi
done

cargo clean --profile "$profile" --package glutin
