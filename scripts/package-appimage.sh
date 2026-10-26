#!/usr/bin/env bash
# Package the release binary as dist/oxidemerge-$VERSION-linux-<arch>.AppImage,
# where <arch> is x64 or arm64.
#
# OxideMerge links only to glibc and opens its display and keyboard libraries
# at runtime. This bundles those libraries, and everything they need. glibc,
# libgcc, and the OpenGL and Vulkan drivers have to match the system, so they
# stay out. assets/AppRun puts a bundled library on the path only when the
# system does not already provide it.
#
# VERSION overrides the version in the file name. CI sets it so untagged
# builds include the commit. Requires Linux on x86_64 or aarch64, curl, and the
# runtime libraries listed below (the same packages the AppImage job installs).

set -euo pipefail

cd "$(dirname "$0")/.."

# The architecture's name in package names, for appimagetool, and in `ldconfig -p`.
# appimagetool is pinned to a release and checked against its SHA-256.
tool_version=1.9.1
case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)
        arch=x64 tool_arch=x86_64 abi=x86-64
        tool_sha256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
        ;;
    Linux-aarch64)
        arch=arm64 tool_arch=aarch64 abi=AArch64
        tool_sha256=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158
        ;;
    *)
        echo "AppImage packages are built on Linux x86_64 or aarch64" >&2
        exit 1
        ;;
esac

if [ -z "${VERSION:-}" ]; then
    VERSION=$(cargo pkgid)
    VERSION=${VERSION##*@}
fi

target_dir=${CARGO_TARGET_DIR:-target}
bin=$target_dir/release/oxidemerge
if [ ! -x "$bin" ]; then
    echo "Missing $bin; run 'make release' first" >&2
    exit 1
fi

name="oxidemerge-$VERSION-linux-$arch"
work=$target_dir/appimage
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
mkdir -p "$app/usr/share/oxidemerge"
echo "$abi" >"$app/usr/share/oxidemerge/abi"

# Libraries OxideMerge links to or opens at runtime (X11, Wayland, and
# keyboard maps through winit), with everything they need.
wanted="libxcb.so.1 libX11.so.6 libX11-xcb.so.1 libXcursor.so.1 libXi.so.6
    libxkbcommon.so.0 libxkbcommon-x11.so.0 libwayland-client.so.0 libwayland-cursor.so.0 libwayland-egl.so.1"
system='^(ld-linux-x86-64|ld-linux-aarch64|libc|libm|libdl|libpthread|librt|libresolv|libutil|libgcc_s|libGL|libEGL|libGLX|libGLdispatch|libOpenGL|libvulkan)\.so'

{
    ldd "$bin"
    for lib in $wanted; do
        # awk exits at the first match, so ldconfig gets SIGPIPE. `|| true` keeps
        # pipefail from aborting the script; a missing library is reported below.
        file=$( { /sbin/ldconfig -p || ldconfig -p; } 2>/dev/null | awk -v name="$lib" -v abi=",$abi" '$1 == name && index($0, abi) { print $NF; exit }' || true)
        if [ -z "$file" ]; then
            echo "${GITHUB_ACTIONS:+::error::}$lib is not installed" >&2
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

tool=$work/appimagetool-$tool_version-$tool_arch.AppImage
if [ ! -x "$tool" ]; then
    curl -fsSL -o "$tool.part" \
        "https://github.com/AppImage/appimagetool/releases/download/$tool_version/appimagetool-$tool_arch.AppImage"
    echo "$tool_sha256  $tool.part" | sha256sum -c - >/dev/null || {
        echo "${GITHUB_ACTIONS:+::error::}appimagetool $tool_version does not match its SHA-256" >&2
        rm -f "$tool.part"
        exit 1
    }
    chmod +x "$tool.part"
    mv "$tool.part" "$tool"
fi
# --appimage-extract works without FUSE. The extracted tool then packs the image.
rm -rf "$work/squashfs-root"
(cd "$work" && "./${tool##*/}" --appimage-extract >/dev/null)
rm -f "dist/$name.AppImage"
ARCH=$tool_arch VERSION="$VERSION" "$work/squashfs-root/AppRun" --no-appstream "$app" "dist/$name.AppImage"

echo "Created dist/$name.AppImage"
