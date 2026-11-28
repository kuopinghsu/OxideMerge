#!/usr/bin/env bash
# Decides whether a new release becomes the repository's latest release on GitHub.
#
#   release-latest.sh <tag> < published-tags   prints true or false
#   release-latest.sh --test                   checks the cases below
#
# The standard input lists the tags of the published releases that are not prereleases, one per
# line (a leading "v" is ignored). <tag> becomes the latest release only when it is a stable
# version (no "-") at least as high as each of them, so a patch for an older line, published
# later, does not take that place. AppImageUpdate resolves "latest" through GitHub's
# /releases/latest, which follows this mark (see scripts/package-appimage.sh).
set -euo pipefail

decide() {
  local v=${1#v} highest
  if [[ $v == *-* ]]; then
    echo false
    return
  fi
  highest=$(sed 's/^v//' | { grep -v -e - -e '^$' || true; } | sort -V | tail -n 1)
  if [ -z "$highest" ] || [ "$(printf '%s\n%s\n' "$highest" "$v" | sort -V | tail -n 1)" = "$v" ]; then
    echo true
  else
    echo false
  fi
}

if [ "${1:-}" = --test ]; then
  failed=0
  check() {
    local want=$1 tag=$2 got
    shift 2
    got=$(printf '%s\n' "$@" | decide "$tag")
    if [ "$got" != "$want" ]; then
      echo "::error::release-latest.sh $tag with releases [$*]: expected $want, got $got" >&2
      failed=1
    fi
  }
  check true v0.9.12 v0.9.10 v0.9.11       # the next patch
  check false v0.9.13 v1.0.0 v0.9.12       # a patch for an older line, after 1.0.0
  check true v1.0.0                        # the first release
  check true v0.10.0 v0.9.11 v0.9.9        # 0.10 is above 0.9, not below it
  check true v1.0.0 v0.9.11 1.0.0-rc1      # a prerelease tag listed by mistake is ignored
  check false v1.0.0-rc1 v0.9.11           # a prerelease is never the latest release
  check false v1.0.0-rc2                   # not even the first one
  check true v0.9.11 v0.9.11               # the same version (the release job refuses it before)
  [ "$failed" = 0 ] && echo "release-latest.sh: all cases pass"
  exit "$failed"
fi

decide "${1:?usage: $0 <tag> < published-tags, or $0 --test}"
