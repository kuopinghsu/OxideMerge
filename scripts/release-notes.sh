#!/usr/bin/env bash
# Prints the CHANGELOG.md section of a version, without its heading, for use
# as release notes. Fails when the version has no section or it is empty.
#
#   scripts/release-notes.sh 0.9.2 [CHANGELOG.md]
set -euo pipefail

version=${1:?usage: $0 VERSION [CHANGELOG]}
version=${version#v}
changelog=${2:-CHANGELOG.md}

notes=$(awk -v version="$version" '
  /^## / {
    if (found) exit
    heading = $2
    gsub(/\[|\]/, "", heading)
    found = heading == version
    next
  }
  !found || /^\[.+\]: / { next }
  /^[[:space:]]*$/ { if (n) blank++; next }
  { while (blank) { lines[++n] = ""; blank-- }; lines[++n] = $0 }
  END { for (i = 1; i <= n; i++) print lines[i] }
' "$changelog")

if [ -z "$notes" ]; then
  echo "error: $changelog has no notes for version $version" >&2
  exit 1
fi
printf '%s\n' "$notes"
