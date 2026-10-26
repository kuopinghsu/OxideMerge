#!/usr/bin/env bash
# Builds the user guides (docs/user-guide.adoc and docs/user-guide.zh-TW.adoc) as PDF files:
#
#   scripts/build-user-guide.sh [OUTPUT_DIR]     # default dist/
#
# writes oxidemerge-<version>-user-guide.pdf and oxidemerge-<version>-user-guide.zh-TW.pdf.
# Needs asciidoctor-pdf, curl, Python 3 with fontTools ($PYTHON, default python3) and the
# DejaVu Sans fonts, found with fc-match. Noto Sans TC, for the Chinese text, is downloaded
# once into target/guide-fonts, checked against its SHA-256.
set -euo pipefail

cd "$(dirname "$0")/.."
out=${1:-dist}
python=${PYTHON:-python3}
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
fonts=$PWD/target/guide-fonts

# The variable font of Google Fonts, at the commit that last changed it.
noto_url=https://raw.githubusercontent.com/google/fonts/b950a7257470b900078f2bf3223823a8602de7e1/ofl/notosanstc/NotoSansTC%5Bwght%5D.ttf
noto_sha256=864727d210d54f2537bbe23b3a839436c3992af72de9322af5270897246bd44f

mkdir -p "$fonts" "$out"
if [ ! -f "$fonts/NotoSansTC-Regular.ttf" ] || [ ! -f "$fonts/NotoSansTC-Bold.ttf" ]; then
  variable=$fonts/NotoSansTC-variable.ttf
  curl -fsSL --retry 3 -o "$variable" "$noto_url"
  echo "$noto_sha256  $variable" | shasum -a 256 -c -
  # asciidoctor-pdf can't pick a weight of a variable font; it gets one font per weight.
  "$python" -m fontTools.varLib.instancer "$variable" wght=400 -q -o "$fonts/NotoSansTC-Regular.ttf"
  "$python" -m fontTools.varLib.instancer "$variable" wght=700 -q -o "$fonts/NotoSansTC-Bold.ttf"
  rm "$variable"
fi
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
  # A character missing from every font is a warning, which fails the build.
  asciidoctor-pdf --failure-level WARN \
    -a pdf-themesdir="$PWD/docs/pdf" -a pdf-theme="$theme" \
    -a pdf-fontsdir="$fonts;GEM_FONTS_DIR" \
    -a revnumber="$version" \
    -o "$pdf" "docs/$doc.adoc"
  echo "Built $pdf"
done
