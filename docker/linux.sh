#!/usr/bin/env bash
# Runs inside the linux test container (scripts/docker-test.sh starts it). As root, it starts
# the test servers (docker/servers.sh), then runs the checks as the user `tester`.
set -euo pipefail
. /src/docker/common.sh
. /src/docker/servers.sh

if [ "$(id -u)" = 0 ]; then
  start_servers
  exec runuser -u tester -- env -i PATH="$PATH" HOME=/home/tester USER=tester LOGNAME=tester \
    LANG="$LANG" LC_ALL="$LC_ALL" CARGO_HOME="$CARGO_HOME" RUSTUP_HOME="$RUSTUP_HOME" \
    CARGO_TARGET_DIR="$CARGO_TARGET_DIR" CARGO_TERM_COLOR="$CARGO_TERM_COLOR" \
    LIBGL_ALWAYS_SOFTWARE=1 GUI_SMOKE_STRICT=1 ${CARGO_BUILD_JOBS:+CARGO_BUILD_JOBS=$CARGO_BUILD_JOBS} \
    OXIDEMERGE_TEST_PASSWORD="$test_password" bash "$0"
fi

cd /work
copy_source
copy_registry
rustc --version

server_tests() {
  pin_ssh_servers
  cargo test --offline --locked --release --bin oxidemerge -- --ignored local_ --test-threads=1
}

# Release builds, as continuous integration tests them.
step clippy cargo clippy --offline --locked --all-targets -- -D warnings
step test cargo test --offline --locked --release
step servers server_tests
step release cargo build --offline --locked --release
if [ -x /build/release/oxidemerge ]; then
  step cli-smoke cli_smoke /build/release/oxidemerge
  step gui-smoke bash scripts/gui-smoke.sh /build/release/oxidemerge /out/gui-smoke
fi
keep_server_logs
finish linux
