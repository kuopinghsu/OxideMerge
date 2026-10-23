#!/usr/bin/env bash
# Package target/release/oxidemerge as dist/oxidemerge-$VERSION-linux-x64.AppImage.
#
# OxideMerge links only to glibc and opens its display and keyboard libraries
# at runtime. This bundles those libraries, and everything they need. glibc,
# libgcc, and the OpenGL and Vulkan drivers have to match the system, so they
# stay out. assets/AppRun puts a bundled library on the path only when the
# system does not already provide it.
#
# VERSION overrides the version in the file name. CI sets it so untagged
# builds include the commit. Requires Linux x86_64, curl, and the runtime
# libraries listed below (the same packages the AppImage job installs).

set -euo pipefail

cd "$(dirname "$0")/.."

if [ "$(uname -s)" != Linux ] || [ "$(uname -m)" != x86_64 ]; then
    echo "AppImage packages are built on Linux x86_64" >&2
    exit 1
fi

if [ -z "${VERSION:-}" ]; then
    VERSION=$(cargo pkgid)
    VERSION=${VERSION##*@}
fi

bin=target/release/oxidemerge
if [ ! -x "$bin" ]; then
    echo "Missing $bin; run 'make release' first" >&2
    exit 1
fi

name="oxidemerge-$VERSION-linux-x64"
work=target/appimage
app=$work/OxideMerge.AppDir
rm -rf "$app"
mkdir -p dist "$work"

install -Dm 755 "$bin" "$app/usr/bin/oxidemerge"
strip "$app/usr/bin/oxidemerge"
install -Dm 755 assets/AppRun "$app/AppRun"
install -Dm 644 assets/oxidemerge.desktop "$app/oxidemerge.desktop"
install -Dm 644 assets/oxidemerge.png "$app/oxidemerge.png"
install -Dm 644 assets/oxidemerge.png "$app/usr/share/icons/hicolor/256x256/apps/oxidemerge.png"
install -Dm 644 LICENSE "$app/usr/share/doc/oxidemerge/LICENSE"
install -Dm 644 README.md "$app/usr/share/doc/oxidemerge/README.md"
ln -s oxidemerge.png "$app/.DirIcon"

# Libraries OxideMerge links to or opens at runtime (X11, Wayland, and
# keyboard maps through winit), with everything they need.
wanted="libxcb.so.1 libX11.so.6 libX11-xcb.so.1 libXcursor.so.1 libXi.so.6
    libxkbcommon.so.0 libxkbcommon-x11.so.0 libwayland-client.so.0 libwayland-cursor.so.0 libwayland-egl.so.1"
system='^(ld-linux-x86-64|libc|libm|libdl|libpthread|librt|libresolv|libutil|libgcc_s|libGL|libEGL|libGLX|libGLdispatch|libOpenGL|libvulkan)\.so'

{
    ldd "$bin"
    for lib in $wanted; do
        file=$( { /sbin/ldconfig -p || ldconfig -p; } 2>/dev/null | awk -v name="$lib" '$1 == name && /x86-64/ { print $NF; exit }')
        if [ -z "$file" ]; then
            echo "$lib is not installed" >&2
            exit 1
        fi
        echo "$lib $file"
        ldd "$file"
    done
} | awk '$2 ~ /^\// { print $1, $2 } $2 == "=>" && $3 ~ /^\// { print $1, $3 }' | sort -u |
while read -r lib file; do
    if [[ ! $lib =~ $system ]]; then
        install -Dm 644 "$(readlink -f "$file")" "$app/usr/lib/oxidemerge/$lib/$lib"
    fi
done

echo "Bundled libraries:"
ls "$app/usr/lib/oxidemerge"

tool=$work/appimagetool-x86_64.AppImage
if [ ! -x "$tool" ]; then
    curl -fsSL -o "$tool" \
        https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage
    chmod +x "$tool"
fi
# --appimage-extract works without FUSE. The extracted tool then packs the image.
rm -rf "$work/squashfs-root"
(cd "$work" && ./appimagetool-x86_64.AppImage --appimage-extract >/dev/null)
rm -f "dist/$name.AppImage"
ARCH=x86_64 VERSION="$VERSION" "$work/squashfs-root/AppRun" --no-appstream "$app" "dist/$name.AppImage"

echo "Created dist/$name.AppImage"
