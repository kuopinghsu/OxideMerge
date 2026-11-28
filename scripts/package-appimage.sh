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
# builds include the commit.
#
# The AppImage carries update information, so AppImageUpdate and similar tools
# can update it from GitHub releases, and appimagetool writes
# dist/$name.AppImage.zsync beside it, to be published with it. The information
# names the GitHub repository kuopinghsu/OxideMerge unless GITHUB_REPOSITORY
# (owner/name, set in GitHub Actions) names another; UPDATE_INFORMATION replaces
# it, and an empty UPDATE_INFORMATION builds an AppImage without it.
#
# Requires Linux on x86_64 or aarch64, curl, and the runtime libraries listed
# below (the same packages the AppImage job installs).

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
    # "…#name@1.2.3", or "…#1.2.3" where the folder has the name of the package.
    VERSION=${VERSION##*[#@]}
fi

target_dir=${CARGO_TARGET_DIR:-target}
# The build tuned for size, as CI packages it (the `release-size` profile).
bin=$target_dir/release-size/oxidemerge
if [ ! -x "$bin" ]; then
    echo "Missing $bin; run 'make opt-size' first" >&2
    exit 1
fi

name="oxidemerge-$VERSION-linux-$arch"
# gh-releases-zsync|owner|repository|release|zsync file. A release version follows "latest":
# AppImageUpdate asks GitHub's /releases/latest, which answers the release marked as latest, and
# the release job marks only the highest stable version (scripts/release-latest.sh). A
# prerelease such as 1.0.0-rc1, or an untagged build such as 0.9.11-abc1234, follows
# "latest-all": AppImageUpdate takes the first entry of /releases, the most recently published
# release, stable or prerelease. It moves to the next prerelease or the stable release, whichever
# comes out first, but also to a patch for an older line published after it. The pattern picks
# this architecture's .zsync from that release.
case $VERSION in
    *-*) channel=latest-all ;;
    *) channel=latest ;;
esac
repo=${GITHUB_REPOSITORY:-kuopinghsu/OxideMerge}
default_info="gh-releases-zsync|${repo%%/*}|${repo#*/}|$channel|oxidemerge-*-linux-$arch.AppImage.zsync"
update_info=${UPDATE_INFORMATION-$default_info}
work=$target_dir/appimage
app=$work/OxideMerge.AppDir
rm -rf "$app"
mkdir -p dist "$work"

install -Dm 755 "$bin" "$app/usr/bin/oxidemerge"
strip "$app/usr/bin/oxidemerge"
install -Dm 755 assets/AppRun "$app/AppRun"
install -Dm 644 assets/oxidemerge.desktop "$app/usr/share/applications/oxidemerge.desktop"
ln -s usr/share/applications/oxidemerge.desktop "$app/oxidemerge.desktop"
# AppStream metadata, for software centres and AppImageHub, with this version as its release.
install -Dm 644 assets/oxidemerge.appdata.xml \
    "$app/usr/share/metainfo/io.github.kuopinghsu.OxideMerge.appdata.xml"
# A Windows working tree can still have CRLF files from before Git's LF rules.
sed -i 's/\r$//' "$app/AppRun" "$app/usr/share/applications/oxidemerge.desktop" \
    "$app/usr/share/metainfo/io.github.kuopinghsu.OxideMerge.appdata.xml"
sed -i "s|<releases/>|<releases><release version=\"$VERSION\" date=\"$(date -u +%Y-%m-%d)\"/></releases>|" \
    "$app/usr/share/metainfo/io.github.kuopinghsu.OxideMerge.appdata.xml"
install -Dm 644 assets/oxidemerge.png "$app/oxidemerge.png"
install -Dm 644 assets/oxidemerge.png "$app/usr/share/icons/hicolor/256x256/apps/oxidemerge.png"
install -Dm 644 LICENSE "$app/usr/share/doc/oxidemerge/LICENSE"
install -Dm 644 README.md "$app/usr/share/doc/oxidemerge/README.md"
ln -s oxidemerge.png "$app/.DirIcon"
mkdir -p "$app/usr/share/oxidemerge"
echo "$abi" >"$app/usr/share/oxidemerge/abi"

# Libraries OxideMerge opens at runtime (X11, Wayland, and keyboard maps through winit), with
# everything they need.
wanted="libXcursor.so.1 libXi.so.6 libxkbcommon.so.0 libxkbcommon-x11.so.0
    libwayland-cursor.so.0 libwayland-egl.so.1"
# Never bundled: glibc, libgcc and libstdc++, the OpenGL and Vulkan drivers, and the base X11
# and Wayland client libraries (libX11, libX11-xcb, libxcb and its libXau, libwayland-client and
# its libffi). Every system with a display has those, and a bundled copy next to the system's
# Mesa or xcb libraries breaks them: AppImage's excludelist (github.com/AppImage/pkg2appimage)
# lists why.
system='^(ld-linux-x86-64|ld-linux-aarch64|libc|libm|libdl|libpthread|librt|libresolv|libutil|libgcc_s|libstdc\+\+|libGL|libEGL|libGLX|libGLdispatch|libOpenGL|libvulkan|libX11|libX11-xcb|libxcb|libXau|libwayland-client|libffi)\.so'

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

# glibc is never bundled, so the newest GLIBC_ version anything needs is the oldest glibc the
# AppImage runs on. RHEL 8 has 2.28; a build on a newer host (Ubuntu 22.04, most WSL
# distributions) stops there with "GLIBC_2.xx not found".
glibc_max=2.28
if ! command -v objdump >/dev/null; then
    echo "${GITHUB_ACTIONS:+::warning::}objdump (binutils) is missing, so the glibc this AppImage needs is not checked." >&2
fi
glibc_needed=$(find "$app/usr" -type f \( -name '*.so*' -o -path '*/bin/*' \) -print0 |
    xargs -0 objdump -T 2>/dev/null | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/GLIBC_//' |
    sort -uV | tail -n 1 || true)
echo "Needs glibc ${glibc_needed:-unknown}"
if [ -n "$glibc_needed" ] &&
    [ "$(printf '%s\n%s\n' "$glibc_max" "$glibc_needed" | sort -V | tail -n 1)" != "$glibc_max" ]; then
    echo "${GITHUB_ACTIONS:+::warning::}This AppImage needs glibc $glibc_needed, so it does not run on RHEL 8 (glibc $glibc_max)." >&2
    echo "Build it in the AlmaLinux 8 container with 'make appimage' (Docker) for RHEL 8." >&2
fi

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
rm -f "dist/$name.AppImage" "dist/$name.AppImage.zsync"
# With update information, appimagetool embeds it and, with its bundled zsyncmake, writes the
# .zsync into the current folder, naming the AppImage beside it. So it runs in dist/.
tool_run=$(cd "$work/squashfs-root" && pwd)/AppRun
app_dir=$(cd "$app" && pwd)
(cd dist && ARCH=$tool_arch VERSION="$VERSION" "$tool_run" --no-appstream \
    ${update_info:+--updateinformation "$update_info"} "$app_dir" "$name.AppImage")

echo "Created dist/$name.AppImage"
if [ -n "$update_info" ]; then
    if [ ! -s "dist/$name.AppImage.zsync" ]; then
        echo "${GITHUB_ACTIONS:+::error::}appimagetool did not write dist/$name.AppImage.zsync" >&2
        exit 1
    fi
    echo "Created dist/$name.AppImage.zsync; update information: $update_info"
fi
