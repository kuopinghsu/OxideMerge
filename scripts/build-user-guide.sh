#!/usr/bin/env bash
# Builds the user guides (docs/user-guide.adoc and docs/user-guide.zh-TW.adoc) as PDF files:
#
#   scripts/build-user-guide.sh [OUTPUT_DIR]     # default dist/ of the repository
#
# writes oxidemerge-<version>-user-guide.pdf and oxidemerge-<version>-user-guide.zh-TW.pdf,
# where <version> is $VERSION or else the version in Cargo.toml. Needs asciidoctor-pdf, curl,
# shasum, Python 3 with fontTools ($PYTHON, default python3), and the DejaVu Sans fonts,
# found with fc-match (fontconfig). Noto Sans TC (the Chinese text), SC and KR (the names of
# the Simplified Chinese and Korean languages) are downloaded once into target/guide-fonts,
# checked against their SHA-256.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
if [ $# -gt 0 ]; then
  mkdir -p "$1"
  out=$(cd "$1" && pwd)
else
  out=$root/dist
fi
cd "$root"
python=${PYTHON:-python3}
version=${VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)}
fonts=$PWD/target/guide-fonts

# The variable fonts of Google Fonts, at the commits that last changed them:
# name, commit of github.com/google/fonts, SHA-256.
noto=(
  "NotoSansTC b950a7257470b900078f2bf3223823a8602de7e1 864727d210d54f2537bbe23b3a839436c3992af72de9322af5270897246bd44f"
  "NotoSansSC 2894aab31764f10f29c421bdfd2340d3b382d384 a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da"
  "NotoSansKR 4efc2774c63917927efe769ca845def6bd6debae 194018e6b2b293a7964f037b25c0249ce1418bc9ab3c971060a03aa57861e252"
)

# asciidoctor-pdf can't pick a weight of a variable font; it gets one font per weight, named
# for it (not for the variable font's default, Thin).
instance=(-m fontTools.varLib.instancer --update-name-table -q)

mkdir -p "$fonts"
for entry in "${noto[@]}"; do
  read -r name commit sha256 <<<"$entry"
  # Written once both weights are made: fonts from another download, other options or an
  # interrupted run are made again.
  stamp=$fonts/$name.made
  made="$sha256 ${instance[*]}"
  if [ "$(cat "$stamp" 2>/dev/null)" = "$made" ]; then
    continue
  fi
  rm -f "$stamp"
  folder=$(echo "$name" | tr '[:upper:]' '[:lower:]')
  variable=$fonts/$name-variable.ttf
  curl -fsSL --retry 3 -o "$variable" \
    "https://raw.githubusercontent.com/google/fonts/$commit/ofl/$folder/$name%5Bwght%5D.ttf"
  echo "$sha256  $variable" | shasum -a 256 -c -
  "$python" "${instance[@]}" "$variable" wght=400 -o "$fonts/$name-Regular.ttf"
  "$python" "${instance[@]}" "$variable" wght=700 -o "$fonts/$name-Bold.ttf"
  rm "$variable"
  echo "$made" >"$stamp"
done
for style in Book:DejaVuSans Bold:DejaVuSans-Bold; do
  file=$(fc-match -f '%{file}' "DejaVu Sans:style=${style%%:*}")
  if [ "$(basename "$file")" != "${style#*:}.ttf" ]; then
    echo "error: DejaVu Sans ${style%%:*} not found (got ${file:-nothing})" >&2
    exit 1
  fi
  cp "$file" "$fonts/"
done

for doc in user-guide user-guide.zh-TW; do
  case $doc in
    *.zh-TW) theme=oxidemerge-zh-TW ;;
    *) theme=oxidemerge ;;
  esac
  pdf=$out/oxidemerge-$version-$doc.pdf
  # A character missing from every font is a warning, which fails the build; asciidoctor-pdf
  # only looks for them when verbose.
  asciidoctor-pdf --verbose --failure-level WARN \
    -a pdf-themesdir="$PWD/docs/pdf" -a pdf-theme="$theme" \
    -a pdf-fontsdir="$fonts;GEM_FONTS_DIR" \
    -a revnumber="$version" -a relfileprefix="oxidemerge-$version-" \
    -o "$pdf" "docs/$doc.adoc"
  echo "Built $pdf"
done
