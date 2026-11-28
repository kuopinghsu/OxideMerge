#!/usr/bin/env bash
# Decides which release is the repository's latest release on GitHub.
#
#   release-latest.sh <tag> < published-tags   prints true or false: whether <tag> becomes it
#   release-latest.sh --highest < published-tags   prints the tag that should be it, if any
#   release-latest.sh --check <tag>            fails unless <tag> is a stable or prerelease tag
#   release-latest.sh --test                   checks the cases below
#
# The standard input lists the tags of the published releases that are not prereleases, one per
# line. A stable tag is v and dotted numbers, such as v1.2.3; a prerelease tag has a "-" after
# them, such as v1.2.3-rc1. Any other tag is refused as <tag> and ignored on the standard input.
# <tag> becomes the latest release only when it is stable and at least as high as each of them,
# so a patch for an older line, published later, does not take that place.
# AppImageUpdate resolves "latest" through GitHub's /releases/latest, which follows this mark
# (see scripts/package-appimage.sh).
set -euo pipefail

usage() {
  echo "usage: $0 <tag> < published-tags, $0 --highest < published-tags, $0 --check <tag>," \
    "or $0 --test" >&2
  exit 2
}

# The one rule for tags, used for <tag> and for the standard input alike.
stable_re='^v?[0-9]+(\.[0-9]+)*$'
prerelease_re='^v?[0-9]+(\.[0-9]+)*-[0-9A-Za-z.-]+$'

# The highest stable tag on the standard input, as it is written there; nothing when there is none.
highest() {
  { grep -E "$stable_re" || true; } |
    awk '{ v = $0; sub(/^v/, "", v); print v "\t" $0 }' | sort -V -k 1,1 | tail -n 1 | cut -f 2
}

# Stops with the usage unless $1 is a stable or a prerelease tag.
check_tag() {
  if [[ ! $1 =~ $stable_re && ! $1 =~ $prerelease_re ]]; then
    echo "${GITHUB_ACTIONS:+::error::}$0: $1 is neither a stable tag (v1.2.3) nor a prerelease tag (v1.2.3-rc1)" >&2
    usage
  fi
}

decide() {
  local v=${1#v} top
  check_tag "$1"
  if [[ $1 =~ $prerelease_re ]]; then
    echo false
    return
  fi
  top=$(highest)
  top=${top#v}
  if [ -z "$top" ] || [ "$(printf '%s\n%s\n' "$top" "$v" | sort -V | tail -n 1)" = "$v" ]; then
    echo true
  else
    echo false
  fi
}

# Read the tags from a pipe or file, never from a terminal someone would have to type into.
need_input() {
  if [ -t 0 ]; then
    echo "$0: give the published tags on the standard input" >&2
    usage
  fi
}

case "${1:-}" in
--test)
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
  check_highest() {
    local want=$1 got
    shift
    got=$(printf '%s\n' "$@" | highest)
    if [ "$got" != "$want" ]; then
      echo "::error::release-latest.sh --highest with releases [$*]: expected '$want', got '$got'" >&2
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
  check_highest v1.0.0 v0.9.13 v1.0.0 v0.9.12
  check_highest v0.10.0 v0.9.11 v0.10.0 v0.9.9
  check_highest v1.0.0 v1.0.0 v1.1.0-rc1
  check_highest ""                         # no release yet
  check_highest v1.1.5 v1.1.5 v1.2.0+1     # a tag of neither kind is ignored
  # A tag of neither kind is refused as <tag>, rather than judged by a different rule.
  for tag in v1.2.0+1 v1.2 v1.2.3.beta; do
    case $tag in
    v1.2) want=true ;;
    *) want=refused ;;
    esac
    got=$(printf 'v1.1.0\n' | decide "$tag" 2>/dev/null) || got=refused
    if [ "$got" != "$want" ]; then
      echo "::error::release-latest.sh $tag: expected $want, got $got" >&2
      failed=1
    fi
  done
  for tag in v1.2.3 v1.2.3-rc1 v1.2.3-alpha.1 v10; do
    (check_tag "$tag") 2>/dev/null || {
      echo "::error::release-latest.sh --check $tag: expected it to pass" >&2
      failed=1
    }
  done
  for tag in v1.2.0+1 v1.2.3- 1.2.3x vx main; do
    if (check_tag "$tag") 2>/dev/null; then
      echo "::error::release-latest.sh --check $tag: expected it to be refused" >&2
      failed=1
    fi
  done
  [ "$failed" = 0 ] && echo "release-latest.sh: all cases pass"
  exit "$failed"
  ;;
--check)
  [ $# = 2 ] || usage
  check_tag "$2"
  ;;
--highest)
  [ $# = 1 ] || usage
  need_input
  highest
  ;;
v[0-9]*)
  [ $# = 1 ] || usage
  need_input
  decide "$1"
  ;;
*)
  usage
  ;;
esac
