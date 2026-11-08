#!/usr/bin/env bash
# Checks OxideMerge on Linux in a Docker container, from any machine with Docker:
#
#   scripts/docker-test.sh
#
# Clippy, the test suite, the tests against real servers (OpenSSH with public keys, host
# certificates, jump hosts and keyboard-interactive through PAM; vsftpd and ProFTPD with FTP,
# FTPS and active mode; Dante and tinyproxy), a release build, and the command line and window
# checks on Xvfb. The tests are built for release, as continuous integration builds them.
#
# Cargo runs offline, from the crates already downloaded into $CARGO_HOME/registry (run
# `cargo fetch --locked` first on a new machine); only building the image needs the network.
# Build output is kept in Docker volumes between runs. Logs go to out/docker/linux/.
# Set CARGO_BUILD_JOBS to build with fewer jobs when Docker has little memory.
set -euo pipefail

cd "$(dirname "$0")/.."
registry=${CARGO_HOME:-$HOME/.cargo}/registry
if [ ! -d "$registry/index" ] || [ ! -d "$registry/cache" ]; then
  echo "docker-test: no downloaded crates in $registry; run cargo fetch --locked first" >&2
  exit 1
fi

rm -rf out/docker/linux
mkdir -p out/docker/linux
docker build -f docker/Dockerfile -t oxidemerge-test-linux docker
status=0
docker run --rm --init -e CARGO_BUILD_JOBS \
  -v "$PWD:/src:ro" -v "$PWD/out/docker/linux:/out" \
  -v "$registry:/hostreg:ro" -v oxidemerge-test-cargo:/usr/local/cargo/registry \
  -v oxidemerge-test-linux-target:/build \
  oxidemerge-test-linux bash /src/docker/linux.sh || status=1
[ "$status" = 0 ] && echo "=== docker-test passed" || echo "=== docker-test failed; logs in out/docker/linux"
exit "$status"
