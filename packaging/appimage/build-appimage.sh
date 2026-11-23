#!/usr/bin/env bash
# Build a distro-agnostic AppImage for Vanta.
#
# This is the packaging format with the most work still to prove out, and
# that is worth saying before the recipe: an AppImage's whole job is to carry
# its own copies of libraries a host might not have at the right version, and
# Vanta links four things a bare `ldd`-following bundler does not fully
# reach -- GTK4, libadwaita and WebKitGTK are ordinary DT_NEEDED dependencies
# that linuxdeploy's ELF walk does find, but WebKitGTK's own helper
# processes (WebKitWebProcess, WebKitNetworkProcess, WebKitGPUProcess), its
# injected bundle, and the bwrap and xdg-dbus-proxy it shells out to for its
# own sandbox are reached through absolute paths baked into
# libwebkitgtk-6.0.so, and GSettings schemas are found by path rather than by
# symbol. All of those are bundled below by hand rather than by the bundler.
# Making the baked-in paths *resolve* on a host that has never installed
# WebKitGTK is AppRun's job, and it is a mount namespace rather than an
# environment variable, because WebKitGTK 2.52 offers no environment variable
# for them -- AppRun carries the measurement and the offsets.
#
# This paragraph used to end "nobody has launched one yet". Somebody has, on
# 2026-08-27: the shell starts and draws on Fedora 44 (Bluefin, GNOME,
# Wayland), first-run window, profile row, Roblox button. The schema handling
# below survived that; WEBKIT_EXEC_PATH did not.
#
# **And a user hit exactly that on 2026-09-02**, from an AppImage mounted at
# /tmp/.mount_Cordia...: `Failed to spawn child process
# "/usr/libexec/webkitgtk-6.0/WebKitNetworkProcess" (No such file or
# directory)`. Installing WebKitGTK on their host did not help, and could not
# have: only Fedora and its derivatives put the helpers under /usr/libexec.
# Debian and Ubuntu ship them at /usr/lib/x86_64-linux-gnu/webkitgtk-6.0 and
# Arch at /usr/lib/webkitgtk-6.0, so the package installs and the path the
# bundled Fedora library asks for is still empty.
#
# AppRun now makes those paths exist in a mount namespace of its own, and this
# script bundles what goes in them. Measured on 2026-09-02 against an AppImage
# built from this recipe, on a host standing in for one that has never
# installed WebKitGTK -- /usr/libexec an empty tmpfs, /usr/bin a copy of itself
# with bwrap and xdg-dbus-proxy removed: with the wrap, WebKitNetworkProcess
# and WebKitWebProcess both ran out of the image, loaded the image's own
# libwebkitgtk and injected bundle, the sandbox engaged on the planted bwrap,
# and a page finished loading; with VANTA_APPIMAGE_NO_WRAP=1, the same image
# on the same host produced the reported error verbatim. AppRun carries the
# full reading.
#
# Two caveats on that. The binaries inside the tested image came from the
# installed vanta rpm rather than from the cargo build below, because the
# machine it was measured on has no webkitgtk6.0-devel and cannot compile the
# webview feature; and the probe was a WebKitWebView, not vanta-shell's own
# sign-in view. That whole measurement was taken against a Fedora-built
# libwebkitgtk-6.0.so, which is no longer what this script bundles -- see the
# note beside the layout section below and in AppRun for what changed and
# what is now unverified because of it.
#
# **Built inside ubuntu:24.04, not Fedora, since ADR-032.** The base moved
# because the declared glibc floor is 2.39 and Fedora 44 is 2.43: eighteen
# bundled libraries came out needing a glibc no host older than Fedora 44
# had, which is what made v0.13.2's AppImage fail almost everywhere. Fedora
# was the base in the first place only because gtk4 was pinned at the `v4_20`
# Cargo feature; the code needs 4.12, and Ubuntu 24.04 carries 4.14.5 and
# libadwaita 1.5.0 -- see ADR-032 for why 22.04 (GTK 4.6) does not clear that
# bar either. The rest of this comment describes a Fedora 44 build and is
# kept for its reasoning, not as a description of what runs today.
#
# Usage:
#     packaging/appimage/build-appimage.sh [--outdir DIR]
#
# Needs: cargo, clang/clang++, cmake, pkg-config, the GTK4/libadwaita/
# WebKitGTK development headers, rsvg-convert (for the AppDir's PNG icon --
# AppImage integration tooling looks for one at the AppDir root even though
# Vanta's own icon is scalable SVG everywhere else), patchelf (the host's,
# not linuxdeploy's -- see the NO_STRIP/PATCHELF block below for what its
# bundled 0.15 does to a library with a .relr.dyn section), and network access
# to fetch linuxdeploy and appimagetool on first use.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(git -C "$here" rev-parse --show-toplevel)
outdir="$repo/dist/appimage"
while [ $# -gt 0 ]; do
    case "$1" in
        --outdir) outdir=$2; shift 2 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

cd "$repo"
eval "$(packaging/version.sh)"
echo "==> building vanta ${VANTA_DESCRIBE}"

need() {
    command -v "$1" >/dev/null 2>&1 || {
        echo "error: $1 is not installed" >&2
        exit 1
    }
}
for tool in cargo clang rsvg-convert readelf glib-compile-schemas patchelf python3; do
    need "$tool"
done

# Where cargo will actually put the binaries. This used to be spelled `target/`
# at every use, which is right only when nothing exported CARGO_TARGET_DIR --
# and CLAUDE.md tells every agent working in a worktree to export one, because
# two builds sharing a target/ once produced two rlibs neither of which held a
# symbol plainly in the source. With one exported, cargo wrote into it and the
# very next line here read `target/release/vanta-run`, which did not exist.
target_dir="${CARGO_TARGET_DIR:-$repo/target}"

tools_dir="${VANTA_APPIMAGE_TOOLS_DIR:-$target_dir/appimage-tools}"
mkdir -p "$tools_dir"

# The architecture this container is actually running on, not the one Vanta
# is being cross-packaged for -- there is no cross-packaging here, linuxdeploy
# and appimagetool both run as ordinary binaries on the build host, and each
# ships a separate AppImage per architecture with no universal build. Fixed to
# the two names both projects' own release pages spell this way -- `uname -m`
# already returns exactly `x86_64` or `aarch64`, which is also how linuxdeploy
# and appimagetool name their own release assets, so no translation table is
# needed the way Debian's amd64/arm64 spelling needs one elsewhere in
# packaging/.
appimage_arch=$(uname -m)
case "$appimage_arch" in
    x86_64)
        linuxdeploy_sha256=c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
        appimagetool_sha256=46fdd785094c7f6e545b61afcfb0f3d98d8eab243f644b4b17698c01d06083d1
        ;;
    aarch64)
        # Computed by hand against the actual release assets the same way the
        # x86_64 pair above was, not copied from anywhere that publishes them:
        # linuxdeploy/linuxdeploy releases/download/1-alpha-20251107-1/linuxdeploy-aarch64.AppImage
        # and AppImage/appimagetool releases/download/1.9.0/appimagetool-aarch64.AppImage,
        # fetched and hashed on 2026-09-23. Bump alongside the x86_64 sums if
        # either tool's pinned version ever changes.
        linuxdeploy_sha256=620095110d693282b8ebeb244a95b5e911cf8f65f76c88b4b47d16ae6346fcff
        appimagetool_sha256=04f45ea45b5aa07bb2b071aed9dbf7a5185d3953b11b47358c1311f11ea94a96
        ;;
    *)
        echo "error: no pinned linuxdeploy/appimagetool build for architecture '$appimage_arch'" >&2
        exit 1
        ;;
esac

fetch_pinned() {
    # A fixed release and its own sha256, computed by hand against the file
    # this pins and checked in here rather than trusted from a remote
    # checksums file, because neither AppImage/appimagetool nor
    # linuxdeploy/linuxdeploy publishes one on their release pages -- GitHub's
    # own asset listing carries a size and a download count, nothing more.
    # Bump the version and the sum together if either tool is ever updated.
    local url=$1 sha256=$2 dest=$3
    if [ -f "$dest" ] && echo "${sha256}  ${dest}" | sha256sum --check --status; then
        return 0
    fi
    curl --fail --location --retry 3 --output "$dest" "$url"
    echo "${sha256}  ${dest}" | sha256sum --check --status || {
        echo "error: $dest does not match the pinned checksum" >&2
        exit 1
    }
    chmod +x "$dest"
}

fetch_pinned \
    "https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-${appimage_arch}.AppImage" \
    "$linuxdeploy_sha256" \
    "$tools_dir/linuxdeploy-${appimage_arch}.AppImage"
fetch_pinned \
    "https://github.com/AppImage/appimagetool/releases/download/1.9.0/appimagetool-${appimage_arch}.AppImage" \
    "$appimagetool_sha256" \
    "$tools_dir/appimagetool-${appimage_arch}.AppImage"

# Both tools are themselves AppImages, and this normally needs FUSE to mount.
# A container has none, so this extracts and runs instead -- the same
# variable mocktail's own packages.yml sets around the equivalent step.
export APPIMAGE_EXTRACT_AND_RUN=1

# linuxdeploy carries its own binutils from 2020 and its own patchelf from
# 2022, and Fedora 44 emits a `.relr.dyn` (SHT_RELR, type 0x13) section in
# every system library. Neither bundled tool knows that section type, and both
# fail in a way that reads as something else entirely:
#
#   strip 2.35    -- "unknown type [0x13] section `.relr.dyn'" on 167 of the
#                    bundled libraries, and linuxdeploy exits 1 on the first.
#   patchelf 0.15 -- succeeds, and relocates .init to the end of the file
#                    without updating DT_INIT. 161 of 165 bundled .so files
#                    came out with DT_INIT still naming the old address; the
#                    loader called libcbor's at base+0x2cc, which is now the
#                    ELF header, and the AppImage took SIGSEGV in call_init
#                    before main. Nothing printed. It looked like a crash in
#                    Vanta.
#
# NO_STRIP costs nothing for the bundled libraries -- Fedora's are already
# stripped and their debug info lives in separate debuginfo packages. **It cost
# a great deal for ours, and this comment used to say otherwise.** Vanta's own
# two binaries carry full DWARF by `[profile.release]`, so leaving linuxdeploy's
# stripping off left roughly 350 MB of symbol tables inside the AppImage --
# which is why it was 176 MB against the rpm's 6. They are stripped above,
# before linuxdeploy runs, which is the right place: the reason to disable its
# stripping is what its 2020 binutils does to a `.relr.dyn` section, and that
# reason has nothing to do with our output. $PATCHELF is
# linuxdeploy's own override; the host's patchelf 0.18 leaves .init at 0x2cc
# and DT_INIT agreeing with it, checked on the same libcbor both ways.
export NO_STRIP=1
export PATCHELF="${PATCHELF:-$(command -v patchelf)}"

export CC=clang CXX=clang++
# `VANTA_GIT_SHA`, not a version. `Cargo.toml` is the version now and a
# packager may not override it -- a release job that stamped its own would
# be the second, disagreeing number this scheme exists to remove. The build
# happens outside a git checkout here, so the commit has to be passed in.
export VANTA_GIT_SHA="$VANTA_SHORTHASH"

# Both crates' `webview` features, never one alone -- see the identical
# comment in packaging/rpm/vanta.spec's %build and packaging/deb/build-deb.sh
# for the shape of the bug that taught this project to say so at every
# callsite: with only one crate's feature on, the linker collects
# webview::open silently and the binary carries no WebKitGTK, with no error
# anywhere in the build. `vanta-runtime/vr` is Play in VR (ADR-053), and
# why the build needs Boost's headers.
cargo build --release --locked \
    --features vanta-shell/webview,vanta-runtime/webview,vanta-runtime/vr

readelf -d "$target_dir/release/vanta-run" | grep -qi webkit || {
    echo "vanta-run linked no WebKitGTK; the webview features did not take" >&2
    exit 1
}

appdir="$target_dir/appimage/AppDir"
rm -rf "$appdir"
mkdir -p "$appdir"

install -Dm755 "$target_dir/release/vanta-shell" "$appdir/usr/bin/vanta-shell"
# **`vanta` is the command; `vanta-shell` is the file.** Asked for on
# 2026-08-28: nobody wants to type the second word, and every other launcher on
# a desktop answers to its own name. A symlink rather than a rename so that
# anything already invoking `vanta-shell` -- a .desktop file somebody edited,
# a script, a bug report -- keeps working, and so the two binaries stay
# obviously related in `ls /usr/bin`. `vanta-run` deliberately gets no alias:
# it is the loader the shell launches and is not what anyone should run by hand.
ln -sf vanta-shell "$appdir/usr/bin/vanta"
# First-party plugins, read-only beside the binary.
# Until the native packages existed nothing installed these anywhere, so the settings window listed no built-in plugins for anybody -- including Flatpak users, whose /app/share/vanta/plugins the code has looked in from the start and which has never existed.
for plugin in plugins/*/; do
    id=$(basename "$plugin")
    [ -f "$plugin/plugin.json" ] || continue
    install -Dm644 "$plugin/plugin.json" "$appdir/usr/share/vanta/plugins/$id/plugin.json"
    install -Dm644 "$plugin/main.ts"     "$appdir/usr/share/vanta/plugins/$id/main.ts"
done

install -Dm755 "$target_dir/release/vanta-run"   "$appdir/usr/bin/vanta-run"

# **Strip our own two binaries, which are almost entirely debug info.**
# `[profile.release]` in Cargo.toml sets `debug = true`, deliberately -- AGENTS.md
# leans on lldb and gdb against a running client and says so at length, and a
# runtime you cannot get a backtrace out of is not worth the disk it saves. But
# that is an argument about the build on a developer's machine, not about what a
# user downloads: `vanta-run` is 207.4 MB unstripped and 15.7 MB with
# `--strip-debug`, measured here, and `vanta-shell` is another 175.7 MB.
#
# Stripping at packaging time keeps both: full DWARF where somebody is debugging,
# and a package that is not thirteen times larger than it needs to be. rpmbuild
# and makepkg already do this by themselves, which is the whole reason the rpm
# and the Arch package were a tenth the size of the others.
#
# Done here, with the host's strip, before linuxdeploy sees the AppDir. NO_STRIP
# below turns off linuxdeploy's own stripping because its bundled binutils is
# from 2020 and mangles a `.relr.dyn` section -- but that is a reason to keep it
# away from Fedora's libraries, not a reason to ship ours unstripped.
strip --strip-debug "$appdir/usr/bin/vanta-shell" "$appdir/usr/bin/vanta-run"

install -Dm644 packaging/io.github.gpt4omni.Vanta.desktop \
    "$appdir/usr/share/applications/io.github.gpt4omni.Vanta.desktop"
install -Dm644 packaging/io.github.gpt4omni.Vanta.metainfo.xml \
    "$appdir/usr/share/metainfo/io.github.gpt4omni.Vanta.metainfo.xml"
install -Dm644 packaging/icons/hicolor/scalable/apps/io.github.gpt4omni.Vanta.svg \
    "$appdir/usr/share/icons/hicolor/scalable/apps/io.github.gpt4omni.Vanta.svg"
install -Dm644 packaging/icons/hicolor/scalable/apps/io.github.gpt4omni.Vanta.Frostbite.svg \
    "$appdir/usr/share/icons/hicolor/scalable/apps/io.github.gpt4omni.Vanta.Frostbite.svg"

licdir="$appdir/usr/share/licenses/vanta"
install -Dm644 LICENSE "$licdir/LICENSE"
install -Dm644 NOTICE "$licdir/NOTICE"
install -Dm644 THIRD-PARTY-NOTICES.md "$licdir/THIRD-PARTY-NOTICES.md"
install -Dm644 third_party/libbadcpu/LICENSE.upstream "$licdir/libbadcpu-MIT.txt"
install -Dm644 third_party/mcpelauncher-linker/LICENSE "$licdir/mcpelauncher-linker-MIT.txt"
install -Dm644 third_party/mcpelauncher-linker/core/NOTICE "$licdir/aosp-NOTICE.txt"
install -Dm644 third_party/libjnivm/LICENSE "$licdir/libjnivm-MIT.txt"
install -Dm644 third_party/mocktail-webview/LICENSE "$licdir/mocktail-webview-Apache-2.0.txt"
# dynarmic and its compiled-in externals, the VR mode's translator (x86-64).
install -Dm644 third_party/dynarmic/LICENSE.txt "$licdir/dynarmic-0BSD.txt"
install -Dm644 third_party/dynarmic/externals/fmt/LICENSE "$licdir/fmt-MIT.txt"
install -Dm644 third_party/dynarmic/externals/mcl/LICENSE "$licdir/mcl-MIT.txt"
install -Dm644 third_party/dynarmic/externals/robin-map/LICENSE "$licdir/robin-map-MIT.txt"
install -Dm644 third_party/dynarmic/externals/xbyak/COPYRIGHT "$licdir/xbyak-BSD-3-Clause.txt"
install -Dm644 third_party/dynarmic/externals/zydis/LICENSE "$licdir/zydis-MIT.txt"
install -Dm644 third_party/dynarmic/externals/zycore/LICENSE "$licdir/zycore-MIT.txt"
# bwrap and xdg-dbus-proxy are whole programs this AppImage now redistributes,
# not libraries swept up by a dependency walk, so their licences travel with
# them. Both are LGPL and both ship a COPYING under /usr/share/licenses.
for pkg in bubblewrap:bwrap-LGPL.txt xdg-dbus-proxy:xdg-dbus-proxy-LGPL.txt; do
    src=/usr/share/licenses/${pkg%%:*}/COPYING
    [ -f "$src" ] && install -Dm644 "$src" "$licdir/${pkg##*:}" || true
done

install -Dm755 packaging/appimage/AppRun "$appdir/AppRun"

# AppImage integration tooling (and appimagetool's own validation) wants a
# desktop file and an icon at the AppDir root, not only under usr/share/. A
# copy rather than a symlink, because appimagetool refuses to package a
# symlink pointing outside the tree it is squashing.
cp "$appdir/usr/share/applications/io.github.gpt4omni.Vanta.desktop" \
    "$appdir/io.github.gpt4omni.Vanta.desktop"
# Rasterised because AppImage's own integration (and thumbnailers that read
# AppImages without extracting them) commonly assume a PNG at the root even
# where the desktop's Icon= key resolves an SVG everywhere else -- 256x256
# matches the largest size Vanta's own icon theme directory would carry had
# one been rendered, and is large enough not to look soft in a file manager.
rsvg-convert --width 256 --height 256 \
    packaging/icons/hicolor/scalable/apps/io.github.gpt4omni.Vanta.svg \
    -o "$appdir/io.github.gpt4omni.Vanta.png"

echo "==> bundling shared libraries with linuxdeploy"
# Plain linuxdeploy, deliberately with no GTK plugin. linuxdeploy-plugin-gtk
# targets GTK3's module and schema layout; this workspace is GTK4 and
# libadwaita, which the plugin does not know how to bundle, and using it
# for the wrong toolkit version risks bundling GTK3 pieces alongside GTK4
# ones rather than helping. The library discovery linuxdeploy does on its
# own -- walking each --executable's ELF dependencies and copying what it
# finds into usr/lib, then rewriting rpaths -- is toolkit-agnostic and is
# the part actually needed here; GSettings schemas and the WebKitGTK helper
# binaries, which that walk cannot see because neither is a DT_NEEDED entry,
# are handled by hand below instead.
# `rpm -ql` was the only lookup this needed while the base was Fedora 44. It
# is not the base any more (ADR-032), and there is no rpm command in an
# Ubuntu container to fall back to -- so this asks whichever package manager
# is actually present. On Ubuntu 24.04 `libwebkitgtk-6.0-4` owns one
# directory for both purposes, `/usr/lib/x86_64-linux-gnu/webkitgtk-6.0`, with
# `injected-bundle` nested inside it; the two greps below still separate them
# because the `find -maxdepth 1` calls further down rely on that separation
# to avoid installing the bundle's .so as if it were a helper executable.
webkit_pkg_files() {
    if command -v rpm >/dev/null 2>&1 && rpm -q webkitgtk6.0 >/dev/null 2>&1; then
        rpm -ql webkitgtk6.0
    elif command -v dpkg-query >/dev/null 2>&1 && dpkg-query -L libwebkitgtk-6.0-4 >/dev/null 2>&1; then
        dpkg-query -L libwebkitgtk-6.0-4
    fi
}
# `[a-z0-9_]+-linux-gnu` rather than the literal `x86_64-linux-gnu`: this is
# Debian's multiarch triplet, which names the *build host's* architecture
# (aarch64-linux-gnu on the arm64 runner), not Vanta's own -- the pattern
# generalises to whichever container this actually runs in rather than
# assuming the one it was first measured against.
webkit_libexec=$(webkit_pkg_files | grep -E '/(libexec|lib64|lib/[a-z0-9_]+-linux-gnu)/webkitgtk-6.0$' | head -1)
webkit_bundle=$(webkit_pkg_files | grep -m1 '/webkitgtk-6.0/injected-bundle$' || true)
deploy_args=(--executable "$appdir/usr/bin/vanta-shell" --executable "$appdir/usr/bin/vanta-run")
if [ -n "$webkit_libexec" ] && [ -d "$webkit_libexec" ]; then
    # Every helper binary passed as its own --executable, not just copied,
    # so linuxdeploy's dependency walk covers *their* DT_NEEDED entries too
    # -- a WebProcess is a separate executable with its own library needs,
    # not merely a data file sitting next to vanta-run.
    while IFS= read -r -d '' helper; do
        deploy_args+=(--executable "$helper")
    done < <(find "$webkit_libexec" -maxdepth 1 -type f -executable -print0)
else
    # Never make a stub lie: better a loud build failure than an AppImage
    # that silently ships a web view with no process to run it in, which is
    # exactly the "webview doesnt work" shape this project has already hit
    # once from the Flatpak missing a Cargo feature rather than a binary.
    echo "error: could not find webkitgtk-6.0's libexec directory (WebKitWebProcess, WebKitNetworkProcess)" >&2
    echo "  the AppImage's web view would have no process to run in" >&2
    exit 1
fi

# The injected bundle: a .so the WebProcess dlopens, from a second absolute
# path baked into libwebkitgtk-6.0.so (/usr/lib64/webkitgtk-6.0/injected-bundle
# on Fedora, /usr/lib/x86_64-linux-gnu/webkitgtk-6.0/injected-bundle on Debian).
# --library rather than --executable because it is dlopened, not exec'd, and
# linuxdeploy's walk still needs to see it or its own dependencies go
# unbundled.
if [ -n "$webkit_bundle" ] && [ -d "$webkit_bundle" ]; then
    while IFS= read -r -d '' so; do
        deploy_args+=(--library "$so")
    done < <(find "$webkit_bundle" -maxdepth 1 -name '*.so' -print0)
else
    echo "error: could not find webkitgtk-6.0's injected-bundle directory" >&2
    echo "  the AppImage's web processes would have no bundle to load" >&2
    exit 1
fi

# bwrap and xdg-dbus-proxy. WebKitGTK 6.0 has no API to turn its sandbox off
# -- the 4.x webkit_web_context_set_sandbox_enabled is gone -- and the sandbox
# is not in-process: the library execs /usr/bin/bwrap, and /usr/bin/xdg-dbus-proxy
# to filter the session bus, both by absolute path, both baked in beside the
# helper path above. The webkitgtk6.0 rpm Requires both by name, so a container
# with the WebKitGTK devel headers has them by construction; a *user's* machine
# that never installed WebKitGTK has neither, which is the whole case this
# AppImage is for. Passed as --executable so their own DT_NEEDED entries
# (libcap, libselinux) are bundled -- a planted binary that cannot start is
# the same gap one level down.
for tool in bwrap xdg-dbus-proxy; do
    tool_path=$(command -v "$tool" 2>/dev/null || true)
    if [ -z "$tool_path" ]; then
        echo "error: $tool is not installed" >&2
        echo "  WebKitGTK execs it by absolute path for its own sandbox, and the" >&2
        echo "  AppImage would carry no copy to plant on a host that lacks one" >&2
        exit 1
    fi
    deploy_args+=(--executable "$tool_path")
done

"$tools_dir/linuxdeploy-${appimage_arch}.AppImage" \
    --appdir "$appdir" \
    "${deploy_args[@]}" \
    --desktop-file "$appdir/io.github.gpt4omni.Vanta.desktop" \
    --icon-file "$appdir/io.github.gpt4omni.Vanta.png"

echo "==> laying out what WebKitGTK reaches by absolute path"
# linuxdeploy was just given each helper as an --executable so their own
# library dependencies land in usr/lib, but linuxdeploy places binaries named
# as --executable next to usr/bin, not back where WebKitGTK's ProcessLauncher
# expects to find them. The layout below is not decoration: AppRun binds
# usr/libexec/webkitgtk-6.0 over /usr/libexec/webkitgtk-6.0 in a mount
# namespace of its own, and libwebkitgtk-6.0.so.4 is patched below so that
# Ubuntu's multiarch path (/usr/lib/<arch>-linux-gnu/webkitgtk-6.0 and its
# /injected-bundle/ subdir) resolves under /usr/libexec/webkitgtk-6.0 on every
# distribution (Arch, Fedora, Ubuntu, Debian, openSUSE, NixOS).
#
#   usr/libexec/webkitgtk-6.0                  -> /usr/libexec/webkitgtk-6.0
#   usr/libexec/webkitgtk-6.0/injected-bundle  -> /usr/libexec/webkitgtk-6.0/injected-bundle
#   usr/bin/bwrap, usr/bin/xdg-dbus-proxy      -> /usr/bin/..., planted only
#                                                 on a host that has none
install -d "$appdir/usr/libexec/webkitgtk-6.0"
while IFS= read -r -d '' helper; do
    name=$(basename "$helper")
    install -m755 "$helper" "$appdir/usr/libexec/webkitgtk-6.0/$name"
    patchelf --set-rpath '$ORIGIN/../../lib' "$appdir/usr/libexec/webkitgtk-6.0/$name"
    # Remove the duplicate copy linuxdeploy dropped into usr/bin/.
    rm -f "$appdir/usr/bin/$name"
done < <(find "$webkit_libexec" -maxdepth 1 -type f -executable -print0)

install -d "$appdir/usr/libexec/webkitgtk-6.0/injected-bundle"
while IFS= read -r -d '' so; do
    name=$(basename "$so")
    install -m755 "$so" "$appdir/usr/libexec/webkitgtk-6.0/injected-bundle/$name"
    patchelf --set-rpath '$ORIGIN/../../../lib' "$appdir/usr/libexec/webkitgtk-6.0/injected-bundle/$name"
    # Remove the duplicate copy linuxdeploy --library dropped into usr/lib/.
    rm -f "$appdir/usr/lib/$name"
done < <(find "$webkit_bundle" -maxdepth 1 -name '*.so' -print0)
# Keep usr/lib/webkitgtk-6.0/injected-bundle as a relative symlink to the
# canonical copy under usr/libexec/webkitgtk-6.0/injected-bundle.
install -d "$appdir/usr/lib/webkitgtk-6.0"
ln -sfn ../../libexec/webkitgtk-6.0/injected-bundle "$appdir/usr/lib/webkitgtk-6.0/injected-bundle"

# Patch Ubuntu/Debian's baked-in multiarch WebKitGTK paths in-place to
# /usr/libexec/webkitgtk-6.0 (NUL-padded to the exact original byte length so
# ELF section offsets do not move). Both the helper directory string
# (`/usr/lib/<multiarch>/webkitgtk-6.0`) and the injected-bundle string
# (`/usr/lib/<multiarch>/webkitgtk-6.0/injected-bundle/`) are rewritten while
# preserving any trailing suffix.
echo "==> patching multiarch WebKitGTK paths in libwebkitgtk-6.0"
python3 - "$appdir/usr/lib" <<'PYEOF'
import glob
import os
import re
import sys

lib_dir = sys.argv[1]
pattern = re.compile(rb"/usr/lib/[a-z0-9_]+-linux-gnu/webkitgtk-6\.0[^\x00]*")
targets = [
    p for p in sorted(glob.glob(os.path.join(lib_dir, "libwebkitgtk-6.0.so*")))
    if os.path.isfile(p) and not os.path.islink(p)
]
if not targets:
    sys.exit("error: no libwebkitgtk-6.0.so* found in AppDir usr/lib")

for path in targets:
    os.chmod(path, os.stat(path).st_mode | 0o200)
    with open(path, "rb") as f:
        data = f.read()

    rewrites = []

    def repl(match: re.Match) -> bytes:
        old = match.group(0)
        marker = b"/webkitgtk-6.0"
        suffix = old[old.index(marker) + len(marker):]
        new = b"/usr/libexec/webkitgtk-6.0" + suffix
        if len(new) > len(old):
            raise RuntimeError(f"replacement {new!r} longer than original {old!r}")
        padded = new + b"\x00" * (len(old) - len(new))
        rewrites.append((old.decode("ascii"), new.decode("ascii")))
        return padded

    patched = pattern.sub(repl, data)
    if len(patched) != len(data):
        raise RuntimeError(f"size mismatch patching {path}: {len(patched)} != {len(data)}")
    if rewrites:
        with open(path, "wb") as f:
            f.write(patched)
        for old_s, new_s in rewrites:
            print(f"  patched {os.path.basename(path)}: {old_s} -> {new_s}")
    else:
        print(f"  {os.path.basename(path)}: no multiarch webkitgtk-6.0 paths to patch")
PYEOF

# linuxdeploy puts --executable binaries in usr/bin already, but say so
# explicitly rather than depending on that: AppRun tests for these two paths
# by name and silently skips planting them if they are absent, which is
# exactly the kind of quiet gap this file exists to avoid.
for tool in bwrap xdg-dbus-proxy; do
    [ -x "$appdir/usr/bin/$tool" ] || install -m755 "$(command -v "$tool")" "$appdir/usr/bin/$tool"
done
# WebKitGTK also reads its own injected bundle and sandbox profile from
# beside the libexec directory in some layouts; copied best-effort rather
# than gated on, since an absent one is a narrower loss (likely the GPU
# process sandbox) than an absent helper binary is.
webkit_share=$(webkit_pkg_files | grep -m1 '/share/webkitgtk-6.0$' || true)
if [ -n "$webkit_share" ] && [ -d "$webkit_share" ]; then
    install -d "$appdir/usr/share/webkitgtk-6.0"
    cp -a "$webkit_share/." "$appdir/usr/share/webkitgtk-6.0/"
fi

echo "==> bundling gdk-pixbuf loaders, GIO modules, and essential icon themes"
# Ubuntu 24.04's bundled libgdk_pixbuf-2.0.so.0 and libgio-2.0.so.0 hardcode
# /usr/lib/<multiarch>/... module paths that do not exist on non-Debian
# distros (Arch, Fedora, openSUSE, NixOS). Bundle the loaders and modules here
# BEFORE the dependency closure loop so their non-excluded DT_NEEDED deps
# (e.g. librsvg-2.so.2 for SVG icons, libgnutls.so.30 for GIO TLS) are pulled
# into usr/lib automatically.
pixbuf_loaders_src=""
for candidate in \
    "/usr/lib/${appimage_arch}-linux-gnu/gdk-pixbuf-2.0/2.10.0/loaders" \
    "/usr/lib64/gdk-pixbuf-2.0/2.10.0/loaders" \
    "/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders"; do
    if [ -d "$candidate" ]; then
        pixbuf_loaders_src="$candidate"
        break
    fi
done
if [ -n "$pixbuf_loaders_src" ]; then
    install -d "$appdir/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders"
    pixbuf_count=0
    while IFS= read -r -d '' loader; do
        name=$(basename "$loader")
        install -m644 "$loader" "$appdir/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders/$name"
        patchelf --set-rpath '$ORIGIN/../../..' "$appdir/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders/$name"
        pixbuf_count=$((pixbuf_count + 1))
    done < <(find "$pixbuf_loaders_src" -maxdepth 1 -name '*.so' -print0)
    echo "  bundled $pixbuf_count gdk-pixbuf loaders from $pixbuf_loaders_src"
else
    echo "warning: gdk-pixbuf loaders directory not found; SVG/image loading may fail on non-Debian hosts" >&2
fi

gio_modules_src=""
for candidate in \
    "/usr/lib/${appimage_arch}-linux-gnu/gio/modules" \
    "/usr/lib64/gio/modules" \
    "/usr/lib/gio/modules"; do
    if [ -d "$candidate" ]; then
        gio_modules_src="$candidate"
        break
    fi
done
if [ -n "$gio_modules_src" ]; then
    install -d "$appdir/usr/lib/gio/modules"
    gio_count=0
    for mod in libgiognutls.so libdconfsettings.so; do
        if [ -f "$gio_modules_src/$mod" ]; then
            install -m644 "$gio_modules_src/$mod" "$appdir/usr/lib/gio/modules/$mod"
            patchelf --set-rpath '$ORIGIN/../..' "$appdir/usr/lib/gio/modules/$mod"
            gio_count=$((gio_count + 1))
        fi
    done
    echo "  bundled $gio_count GIO modules from $gio_modules_src"
else
    echo "warning: GIO modules directory not found; GIO TLS/dconf may fail on non-Debian hosts" >&2
fi

if [ -f /usr/share/icons/Adwaita/index.theme ]; then
    install -Dm644 /usr/share/icons/Adwaita/index.theme \
        "$appdir/usr/share/icons/Adwaita/index.theme"
    if [ -d /usr/share/icons/Adwaita/symbolic ]; then
        install -d "$appdir/usr/share/icons/Adwaita/symbolic"
        cp -a /usr/share/icons/Adwaita/symbolic/. "$appdir/usr/share/icons/Adwaita/symbolic/"
    fi
    echo "  bundled Adwaita icon theme metadata + symbolic icons"
fi
if [ -f /usr/share/icons/hicolor/index.theme ]; then
    install -Dm644 /usr/share/icons/hicolor/index.theme \
        "$appdir/usr/share/icons/hicolor/index.theme"
fi
icon_cache_tool=$(command -v gtk4-update-icon-cache 2>/dev/null || command -v gtk-update-icon-cache 2>/dev/null || true)
if [ -n "$icon_cache_tool" ]; then
    for theme_dir in "$appdir/usr/share/icons/Adwaita" "$appdir/usr/share/icons/hicolor"; do
        [ -f "$theme_dir/index.theme" ] && "$icon_cache_tool" -q -t -f "$theme_dir" || true
    done
fi

echo "==> pruning host-driver libraries and completing the dependency closure"
# linuxdeploy carries its own fixed list of libraries it assumes every host
# already has, and does not copy them regardless of what --executable or
# --library named -- see its own `excludelist`. Conversely, linuxdeploy DOES
# copy host-driver-coupled libraries (libvulkan, libwayland-*, libX11/libxcb,
# libexpat, libz, libzstd, libelf, libffi, libfontconfig, libfreetype, libudev,
# libsystemd) from the Ubuntu 24.04 build container into usr/lib/. When AppRun
# prepends usr/lib to LD_LIBRARY_PATH, those older Ubuntu copies shadow the
# host's copies that host Mesa (libEGL_mesa, libgallium, Vulkan ICDs), NVIDIA
# drivers, and Fontconfig directly link against -- causing blank canvases,
# WebKitGTK `EGL_BAD_PARAMETER` aborts, and Fontconfig config-syntax warnings
# on Arch/Fedora/openSUSE.
#
# Therefore we:
#   1. Actively DELETE every library on `never_bundle_libs` that linuxdeploy
#      copied into `$appdir/usr/lib/`.
#   2. Compute the remaining DT_NEEDED dependency closure ourselves to a fixed
#      point, respecting `never_bundle_libs`, so genuine app/toolkit deps that
#      linuxdeploy's excludelist dropped (like libharfbuzz or librsvg for the
#      SVG pixbuf loader) are always bundled.
#
# The dynamic linker's own soname is read off vanta-shell's `.interp` section
# rather than hardcoding x86_64 vs aarch64.
ld_linux=$(readelf -p .interp "$appdir/usr/bin/vanta-shell" 2>/dev/null \
    | grep -oE 'ld-linux[a-zA-Z0-9._-]*\.so\.[0-9]+' | head -1)
if [ -z "$ld_linux" ]; then
    echo "error: could not read the ELF interpreter out of vanta-shell's .interp section" >&2
    exit 1
fi
never_bundle_libs="
    libEGL.so.1 libGLX.so.0 libGL.so.1 libOpenGL.so.0 libGLdispatch.so.0 libgbm.so.1 libdrm.so.2 libGLESv2.so.2
    libvulkan.so.1
    libwayland-client.so.0 libwayland-egl.so.1 libwayland-cursor.so.0 libwayland-server.so.0
    libX11.so.6 libX11-xcb.so.1 libxcb.so.1 libxcb-shm.so.0 libxcb-render.so.0
    libXext.so.6 libXfixes.so.3 libXdamage.so.1 libXrandr.so.2 libXrender.so.1
    libXi.so.6 libXcursor.so.1 libXinerama.so.1 libXau.so.6 libXdmcp.so.6
    libexpat.so.1 libz.so.1 libzstd.so.1 libelf.so.1 libffi.so.8
    libpipewire-0.3.so.0 libasound.so.2 libpulse.so.0 libdbus-1.so.3 libudev.so.1 libsystemd.so.0
    libfontconfig.so.1 libfreetype.so.6
    libc.so.6 libm.so.6 libdl.so.2 libpthread.so.0 librt.so.1 libresolv.so.2 libutil.so.1 libnsl.so.1 libanl.so.1 libcrypt.so.1
    $ld_linux libstdc++.so.6 libgcc_s.so.1
"
is_never_bundled() {
    local name="$1" g
    for g in $never_bundle_libs; do
        [ "$name" = "$g" ] && return 0
    done
    return 1
}

pruned=0
for lib in $never_bundle_libs; do
    if [ -e "$appdir/usr/lib/$lib" ] || [ -L "$appdir/usr/lib/$lib" ]; then
        rm -f "$appdir/usr/lib/$lib"
        echo "  pruned host-coupled library: $lib"
        pruned=$((pruned + 1))
    fi
done
echo "  pruned $pruned host-coupled libraries copied by linuxdeploy"

pass=0
added=1
while [ "$added" -gt 0 ]; do
    pass=$((pass + 1))
    added=0
    while IFS= read -r -d '' elf; do
        while IFS= read -r needed; do
            [ -z "$needed" ] && continue
            is_never_bundled "$needed" && continue
            [ -e "$appdir/usr/lib/$needed" ] && continue
            src=$(ldconfig -p 2>/dev/null | awk -v n="$needed" '$1 == n { print $NF; exit }')
            if [ -z "$src" ] || [ ! -e "$src" ]; then
                echo "error: $elf needs $needed, which is neither bundled nor on this build host" >&2
                echo "  linuxdeploy's excludelist dropped it, assuming a host has it -- this one" >&2
                echo "  does not, so the AppImage would fail the same way on a real one" >&2
                exit 1
            fi
            cp -L "$src" "$appdir/usr/lib/$needed"
            chmod 644 "$appdir/usr/lib/$needed"
            patchelf --set-rpath '$ORIGIN' "$appdir/usr/lib/$needed"
            echo "  completed closure: $needed (needed by ${elf#"$appdir"/}, dropped by linuxdeploy's excludelist)"
            added=$((added + 1))
        done < <(patchelf --print-needed "$elf" 2>/dev/null || true)
    done < <(find "$appdir/usr/bin" "$appdir/usr/lib" "$appdir/usr/libexec" \
        -type f \( -executable -o -name '*.so*' \) -print0 2>/dev/null)
    echo "  pass $pass: completed $added"
done

echo "==> generating gdk-pixbuf and GIO module caches"
appdir_abs=$(cd "$appdir" && pwd -P)
if [ -d "$appdir/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders" ]; then
    query_loaders=""
    for candidate in \
        "/usr/lib/${appimage_arch}-linux-gnu/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders" \
        "$(command -v gdk-pixbuf-query-loaders-64 2>/dev/null || true)" \
        "$(command -v gdk-pixbuf-query-loaders 2>/dev/null || true)"; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            query_loaders="$candidate"
            break
        fi
    done
    if [ -n "$query_loaders" ]; then
        loaders_cache_in="$appdir/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache.in"
        LD_LIBRARY_PATH="$appdir_abs/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
        GDK_PIXBUF_MODULEDIR="$appdir_abs/usr/lib/gdk-pixbuf-2.0/2.10.0/loaders" \
            "$query_loaders" \
            | python3 -c 'import sys; print(sys.stdin.read().replace(sys.argv[1], "@APPDIR@"), end="")' "$appdir_abs" \
            > "$loaders_cache_in"
        echo "  generated loaders.cache.in via $query_loaders"
    else
        echo "warning: gdk-pixbuf-query-loaders not found; loaders.cache.in not generated" >&2
    fi
fi

if [ -d "$appdir/usr/lib/gio/modules" ]; then
    gio_query=""
    for candidate in \
        "/usr/lib/${appimage_arch}-linux-gnu/glib-2.0/gio-querymodules" \
        "$(command -v gio-querymodules-64 2>/dev/null || true)" \
        "$(command -v gio-querymodules 2>/dev/null || true)"; do
        if [ -n "$candidate" ] && [ -x "$candidate" ]; then
            gio_query="$candidate"
            break
        fi
    done
    if [ -n "$gio_query" ]; then
        LD_LIBRARY_PATH="$appdir_abs/usr/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
            "$gio_query" "$appdir_abs/usr/lib/gio/modules"
        echo "  generated giomodule.cache via $gio_query"
    elif [ -n "$gio_modules_src" ] && [ -f "$gio_modules_src/giomodule.cache" ]; then
        # Fallback: copy only entries for modules we actually bundled.
        : > "$appdir/usr/lib/gio/modules/giomodule.cache"
        while IFS= read -r line; do
            mod_name=${line%%:*}
            if [ -f "$appdir/usr/lib/gio/modules/$mod_name" ]; then
                printf '%s\n' "$line" >> "$appdir/usr/lib/gio/modules/giomodule.cache"
            fi
        done < "$gio_modules_src/giomodule.cache"
        echo "  filtered giomodule.cache from $gio_modules_src"
    fi
fi

echo "==> compiling GSettings schemas into the AppDir"
# Looked up by GIO through GSETTINGS_SCHEMA_DIR at runtime (see AppRun), not
# discoverable from any binary's DT_NEEDED entries, so linuxdeploy's walk
# above never touches this. Best-effort: GTK4/libadwaita read a handful of
# schemas for things like colour-scheme preference, and their absence is a
# fallback-to-default rather than a crash, which is a materially smaller risk
# than the WebKitGTK helper binaries above -- hence no hard failure here.
schemas_src=/usr/share/glib-2.0/schemas
if [ -d "$schemas_src" ]; then
    install -d "$appdir/usr/share/glib-2.0/schemas"
    cp "$schemas_src"/*.xml "$appdir/usr/share/glib-2.0/schemas/" 2>/dev/null || true
    glib-compile-schemas "$appdir/usr/share/glib-2.0/schemas"
else
    echo "warning: $schemas_src not found; the AppImage ships no compiled GSettings schemas" >&2
fi

echo "==> verifying post-build AppDir invariants"
forbidden_present=()
for lib in $never_bundle_libs; do
    if [ -e "$appdir/usr/lib/$lib" ] || [ -L "$appdir/usr/lib/$lib" ]; then
        forbidden_present+=("$lib")
    fi
done
if [ "${#forbidden_present[@]}" -gt 0 ]; then
    echo "error: forbidden host-driver/system libraries are still present in $appdir/usr/lib/:" >&2
    printf '  %s\n' "${forbidden_present[@]}" >&2
    exit 1
fi

python3 - "$appdir/usr/lib" <<'PYEOF'
import glob
import os
import re
import sys

lib_dir = sys.argv[1]
bad = re.compile(rb"/usr/lib/[a-z0-9_]+-linux-gnu/webkitgtk-6\.0")
good = b"/usr/libexec/webkitgtk-6.0"
targets = [
    p for p in sorted(glob.glob(os.path.join(lib_dir, "libwebkitgtk-6.0.so*")))
    if os.path.isfile(p) and not os.path.islink(p)
]
if not targets:
    sys.exit("error: libwebkitgtk-6.0.so* missing from AppDir usr/lib")
for path in targets:
    with open(path, "rb") as f:
        data = f.read()
    if bad.search(data):
        sys.exit(f"error: {path} still contains unpatched /usr/lib/*-linux-gnu/webkitgtk-6.0 path")
    if good not in data:
        sys.exit(f"error: {path} does not contain expected /usr/libexec/webkitgtk-6.0 path")
PYEOF

for helper in WebKitNetworkProcess WebKitWebProcess; do
    if [ ! -x "$appdir/usr/libexec/webkitgtk-6.0/$helper" ]; then
        echo "error: missing $appdir/usr/libexec/webkitgtk-6.0/$helper" >&2
        exit 1
    fi
    if [ -e "$appdir/usr/bin/$helper" ]; then
        echo "error: duplicate helper left in $appdir/usr/bin/$helper" >&2
        exit 1
    fi
    rpath=$(patchelf --print-rpath "$appdir/usr/libexec/webkitgtk-6.0/$helper" 2>/dev/null || true)
    if [ "$rpath" != '$ORIGIN/../../lib' ]; then
        echo "error: $helper RUNPATH is '$rpath', expected '\$ORIGIN/../../lib'" >&2
        exit 1
    fi
done
if [ -z "$(find "$appdir/usr/libexec/webkitgtk-6.0/injected-bundle" -maxdepth 1 -name '*.so' 2>/dev/null)" ]; then
    echo "error: missing injected-bundle .so in $appdir/usr/libexec/webkitgtk-6.0/injected-bundle" >&2
    exit 1
fi
echo "  all post-build AppDir assertions passed"

echo "==> checking every bundled ELF against the glibc floor"
# **This is the check that already existed and was pointed at the wrong
# files, and v0.13.2 shipped broken because of it.**
#
# `check-glibc-floor.sh` was written after one `log10f@GLIBC_2.43` made
# v0.10.0's rpm uninstallable, and its own header predicted this exact
# outcome: "The AppImage had it too, and that one bundles no libc at all, so
# the format whose whole purpose is running anywhere would have run on Fedora
# 44 and nothing else." The release workflow then ran it over
# `target/release/vanta-run` and `target/release/vanta-shell` and nothing
# else -- so Vanta's own two binaries were held at GLIBC_2.39, and the
# hundred-odd libraries linuxdeploy copies out of this Fedora 44 container
# were never looked at.
#
# Measured on the shipped v0.13.2 AppImage on 2026-09-13: both Vanta
# binaries at GLIBC_2.39, and eighteen bundled libraries at GLIBC_2.43 --
# among them libgtk-4.so.1, libglib-2.0.so.0 and libwebkitgtk-6.0.so.4, which
# is to say the ones nothing can start without. On any host older than Fedora
# 44 the loader refuses them all before `main` and the user sees a wall of
# "version `GLIBC_2.43' not found", one line per library. It reads like
# missing dependencies. Every dependency is present; they are simply built
# against a libc the host has not got, and an AppImage cannot bundle the libc
# that would satisfy them.
#
# So the gate runs over the whole AppDir, here rather than only in CI, because
# this script is also run by hand and a check that only fires in a workflow is
# a check somebody can ship around.
mapfile -t appdir_elves < <(
    find "$appdir" -type f \( -name '*.so' -o -name '*.so.*' -o -perm -u+x \) -print0 \
        | xargs -0 -r file --mime-type -- \
        | awk -F': ' '$2 ~ /application\/x-(sharedlib|executable|pie-executable)/ {print $1}'
)
if [ "${#appdir_elves[@]}" -eq 0 ]; then
    echo "check-glibc-floor: found no ELF files in $appdir, which cannot be right" >&2
    exit 2
fi
echo "    ${#appdir_elves[@]} ELF files to check"
if ! "$repo/packaging/check-glibc-floor.sh" "${appdir_elves[@]}"; then
    cat >&2 <<'FLOOREOF'

The AppImage was not written.

One or more bundled libraries need a newer glibc than the floor, which means
this image would fail to start on every host older than the container it was
built in -- the failure mode v0.13.2 shipped with. The libraries named above
came out of this build container, so the fix is not in Vanta's own code:
either build the image on a base whose glibc is at or below the floor, or
bundle the loader and libc alongside them and exec through it. Do not raise
VANTA_GLIBC_FLOOR to make this pass; that silently drops distributions.
FLOOREOF
    exit 1
fi

echo "==> appimagetool"
outfile="$outdir/Vanta-${VANTA_DESCRIBE}-${appimage_arch}.AppImage"
mkdir -p "$outdir"
ARCH="$appimage_arch" "$tools_dir/appimagetool-${appimage_arch}.AppImage" "$appdir" "$outfile"

chmod +x "$outfile"
ls -lh "$outfile"
echo "built: $outfile"
