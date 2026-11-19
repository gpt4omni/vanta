#!/usr/bin/env bash
# Derive a *package* version from `git describe`, and print it as shell
# assignments.
#
# **This is not where the window title's version comes from, and this comment
# used to say it was.** It claimed AGENTS.md named `git describe --tags
# --always --dirty` as the source for both; the title's version is
# `Cargo.toml`'s and has been since crates/vanta-shell/build.rs stopped
# computing one. What that build script still takes from here is
# `VANTA_GIT_SHA`, the provenance half, which the three native package
# scripts export from VANTA_SHORTHASH below.
#
# A package version needs something the manifest cannot give it: dpkg, rpm and
# pacman all compare versions to decide whether an upgrade is an upgrade, so
# two builds cut between one tag and the next must order, and a bare 0.14.0
# repeated does not. That is what the distance-from-tag component is for, and
# it is why this file keeps deriving from `git describe` rather than reading
# the manifest.
#
# Printed rather than exported, so this is meant to be evaluated rather than
# sourced or run for effect:
#
#     version_vars=$(packaging/version.sh)
#     eval "$version_vars"
#     echo "$VANTA_VERSION"
#
# That is a deliberate choice over `source`-ing it: a sourced script that
# forgets to `export` leaves the caller with nothing and no error, and one
# that changes `set -e` underneath a caller's own shell options is a footgun
# for exactly the reason this project's own justfile avoids it. The separate
# assignment preserves a failed version lookup rather than hiding it in eval.
#
# A new fork has no release tags. Until its first release, use the manifest
# version and total commit count so source packages still order and retain
# their provenance. RPM and Arch use this same derivation.
#
# Sets:
#   VANTA_DESCRIBE   the `git describe --tags --long` string (or manifest
#                      version, commit count and hash without tags) with the
#                      leading v stripped, e.g. 0.7.0-37-gcbd53e5. Used to name
#                      the AppImage and to build the .deb and .rpm versions.
#                      It once fed a VANTA_BUILD_VERSION that no longer
#                      exists; nothing reads that variable now.
#   VANTA_VERSION    the tag or manifest version, e.g. 0.7.0
#   VANTA_COMMITS    commits since that tag, or total commits without tags
#   VANTA_SHORTHASH  the abbreviated commit, e.g. cbd53e5
set -euo pipefail

if ! describe=$(git describe --tags --match 'v[0-9]*' --long --abbrev=7 2>/dev/null); then
    root=$(git rev-parse --show-toplevel)
    version=$(sed -n 's/^version = "\([0-9][^"]*\)".*/\1/p' "$root/Cargo.toml" | head -n1)
    if [ -z "$version" ]; then
        echo "cannot find the workspace version in Cargo.toml" >&2
        exit 1
    fi
    describe="v${version}-$(git rev-list --count HEAD)-g$(git rev-parse --short=7 HEAD)"
fi

version=${describe#v}; version=${version%%-*}
rest=${describe#v${version}-}
commits=${rest%%-*}
shorthash=${rest#*-g}

printf 'VANTA_DESCRIBE=%s\n' "${describe#v}"
printf 'VANTA_VERSION=%s\n' "$version"
printf 'VANTA_COMMITS=%s\n' "$commits"
printf 'VANTA_SHORTHASH=%s\n' "$shorthash"
