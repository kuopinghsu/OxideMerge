#!/usr/bin/env bash
# Builds the AppImage in the AlmaLinux 8 container of docker/appimage.Dockerfile, which
# `make appimage` runs with the checkout mounted at /work.
#
# On a Linux host, `make appimage` passes HOST_UID and HOST_GID: dist/, the AppImage and its
# .zsync, which root in the container writes, are given back to that user, even when the build
# stops after dist/ was made. With rootless Docker or Podman, root in the container already is the user
# outside it, and a chown would hand the files to one of that user's subordinate ids instead,
# so they are left as they are there.

set -euo pipefail

# Whether root here is root outside the container too: the user namespace maps id 0 to 0.
real_root() {
    local inside outside
    read -r inside outside _ </proc/self/uid_map && [ "$inside" = 0 ] && [ "$outside" = 0 ]
}

give_back() {
    local status=$?
    if [ -n "${HOST_UID:-}" ] && [ -d dist ] && real_root; then
        local owner="$HOST_UID:${HOST_GID:-$HOST_UID}"
        chown "$owner" dist || true
        find dist -maxdepth 1 \( -name '*.AppImage' -o -name '*.AppImage.zsync' \) -exec chown "$owner" {} + || true
    fi
    exit "$status"
}
trap give_back EXIT

bash scripts/patch-glutin.sh
cargo build --profile release-size --locked
bash scripts/package-appimage.sh
