#!/usr/bin/env bash
# Vanta universal Linux installer
#
# Usage:
#   curl -fsSL https://gpt4omni.github.io/vanta/install.sh | bash
#   curl -fsSL https://gpt4omni.github.io/vanta/install.sh | bash -s -- --method flatpak
#   curl -fsSL https://gpt4omni.github.io/vanta/install.sh | bash -s -- --dry-run
#
# Auto-detects Linux distribution, CPU architecture (x86_64 / aarch64), glibc
# floor, and immutability, verifies Vanta's pinned OpenPGP signing keys, and
# installs Vanta via the best signed channel for your system:
#   - Flatpak (recommended on immutable/atomic distros & universal fallback; no sudo)
#   - Signed APT repository (Debian 13+ / Ubuntu 24.04+ / Mint 22+ / Pop!_OS 24.04+)
#   - Signed DNF repository (Fedora 44)
#   - Signed Pacman repository (Arch / EndeavourOS / CachyOS / Manjaro / Omarchy, x86_64)
#   - AppImage with desktop entry + roblox:// handler (fallback when Flatpak is absent)

set -euo pipefail

# Pinned Vanta signing key fingerprints (matching docs/install.md)
readonly FLATPAK_GPG_FPR="225CFE57D7BC6D718509D1DAE3C84BF51338D38C"
readonly APT_GPG_FPR="6A43861D03031FEAF8E5BB668DD5F110EFD5EF79"
readonly RPM_GPG_FPR="13CE044B117BAF65814BD4E697EFBC73BAC353D6"
readonly ARCH_GPG_FPR="A75EDC4B9B7383BB2FF0F6BEB5018BF3AB2ACA11"

readonly BASE_URL="https://gpt4omni.github.io/vanta"
readonly REPO_API="https://api.github.com/repos/gpt4omni/vanta/releases/latest"
readonly APP_ID="io.github.gpt4omni.Vanta"
readonly ICON_URL="https://raw.githubusercontent.com/gpt4omni/vanta/main/packaging/icons/hicolor/scalable/apps/${APP_ID}.svg"

METHOD="auto"
CHANNEL="stable"
ASSUME_YES=0
DRY_RUN=0
TMPDIR_WORK=""

# ---- Formatting helpers ----------------------------------------------------

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    C_BOLD=$'\033[1m'
    C_GREEN=$'\033[32m'
    C_YELLOW=$'\033[33m'
    C_RED=$'\033[31m'
    C_CYAN=$'\033[36m'
    C_RESET=$'\033[0m'
else
    C_BOLD="" C_GREEN="" C_YELLOW="" C_RED="" C_CYAN="" C_RESET=""
fi

info()  { printf '%s[vanta]%s %s\n' "${C_CYAN}${C_BOLD}" "${C_RESET}" "$*"; }
ok()    { printf '%s[vanta]%s %s\n' "${C_GREEN}${C_BOLD}" "${C_RESET}" "$*"; }
warn()  { printf '%s[vanta]%s %s\n' "${C_YELLOW}${C_BOLD}" "${C_RESET}" "$*" >&2; }
die()   { printf '%s[vanta]%s error: %s\n' "${C_RED}${C_BOLD}" "${C_RESET}" "$*" >&2; exit 1; }

cleanup() {
    if [ -n "${TMPDIR_WORK:-}" ] && [ -d "$TMPDIR_WORK" ]; then
        rm -rf "$TMPDIR_WORK"
    fi
}
trap cleanup EXIT

usage() {
    cat <<'EOF'
Vanta Linux Installer

Usage:
  install.sh [OPTIONS]

Options:
  --method <mode>     Install method: auto (default), flatpak, apt, dnf, pacman, appimage
  --channel <branch>  Flatpak branch: stable (default) or master (latest main commit)
  -y, --yes           Run non-interactively without confirmation prompts
  --dry-run           Detect OS/architecture and print planned actions without changing system
  -h, --help          Show this help message

Examples:
  curl -fsSL https://gpt4omni.github.io/vanta/install.sh | bash
  curl -fsSL https://gpt4omni.github.io/vanta/install.sh | bash -s -- --method flatpak
  curl -fsSL https://gpt4omni.github.io/vanta/install.sh | bash -s -- --method appimage
EOF
}

run() {
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '  %s[dry-run]%s' "${C_YELLOW}" "${C_RESET}"
        printf ' %q' "$@"
        printf '\n'
        return 0
    fi
    "$@"
}

as_root() {
    if [ "$(id -u)" -eq 0 ]; then
        run "$@"
    elif command -v sudo >/dev/null 2>&1; then
        run sudo "$@"
    elif command -v doas >/dev/null 2>&1; then
        run doas "$@"
    else
        die "Root privileges are required for this install method (no sudo or doas found). Try --method flatpak or --method appimage."
    fi
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "Required command '$1' is not installed."
}

url_ok() {
    curl -fsSLI --connect-timeout 5 --max-time 10 "$1" >/dev/null 2>&1
}

# Verify an OpenPGP public key file contains the exact expected 40-char fingerprint.
verify_gpg_fingerprint() {
    local keyfile="$1"
    local expected="$2"
    if ! command -v gpg >/dev/null 2>&1; then
        warn "gpg is not installed; skipping local fingerprint check before import."
        return 0
    fi
    local found
    found=$(gpg --show-keys --with-colons "$keyfile" 2>/dev/null \
        | awk -F: '$1 == "fpr" { print toupper($10) }')
    if ! printf '%s\n' "$found" | grep -qx "$expected"; then
        die "Signing key fingerprint mismatch! Expected ${expected}, got: ${found:-<none>}"
    fi
    ok "Verified signing key fingerprint: ${expected}"
}

# ---- System detection ------------------------------------------------------

detect_arch() {
    local raw="${VANTA_INSTALL_ARCH:-$(uname -m)}"
    case "$raw" in
        x86_64|amd64)  printf 'x86_64' ;;
        aarch64|arm64)
            # Raspberry Pi OS (32-bit) boots a 64-bit kernel (`uname -m` == aarch64)
            # with a 32-bit armhf userspace. Vanta requires 64-bit arm64 userspace.
            if [ -z "${VANTA_INSTALL_ARCH:-}" ]; then
                local bits="" dpkg_arch=""
                if command -v getconf >/dev/null 2>&1; then
                    bits=$(getconf LONG_BIT 2>/dev/null || true)
                fi
                if command -v dpkg >/dev/null 2>&1; then
                    dpkg_arch=$(dpkg --print-architecture 2>/dev/null || true)
                fi
                if [ "$bits" = "32" ] || [ "$dpkg_arch" = "armhf" ] || [ "$dpkg_arch" = "armel" ]; then
                    die "Detected 32-bit ARM userspace (${dpkg_arch:-32-bit}) on a 64-bit kernel (e.g. Raspberry Pi OS 32-bit). Vanta requires a 64-bit OS such as Raspberry Pi OS (64-bit)."
                fi
            fi
            printf 'aarch64'
            ;;
        *) die "Unsupported CPU architecture: ${raw}. Vanta supports x86_64 and aarch64 Linux." ;;
    esac
}

# Returns 0 if host glibc >= 2.39 (matches Ubuntu 24.04 floor), 1 otherwise.
glibc_meets_floor() {
    local ver=""
    if [ -n "${VANTA_INSTALL_GLIBC:-}" ]; then
        ver="$VANTA_INSTALL_GLIBC"
    elif command -v getconf >/dev/null 2>&1; then
        ver=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}' || true)
    elif command -v ldd >/dev/null 2>&1; then
        ver=$(ldd --version 2>&1 | head -n1 | grep -oE '[0-9]+\.[0-9]+' | tail -n1 || true)
    fi
    [ -n "$ver" ] || return 1
    local major minor
    major="${ver%%.*}"
    minor="${ver#*.}"
    minor="${minor%%.*}"
    if [ "$major" -gt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -ge 39 ]; }; then
        return 0
    fi
    return 1
}

OS_ID="unknown"
OS_ID_LIKE=""
OS_VERSION_ID=""
OS_PRETTY="Linux"
IS_IMMUTABLE=0

detect_os() {
    local os_release="${VANTA_INSTALL_OS_RELEASE:-/etc/os-release}"
    if [ -r "$os_release" ]; then
        # Read only known key=value fields without executing arbitrary shell code.
        while IFS='=' read -r k v; do
            v="${v#\"}"; v="${v%\"}"
            v="${v#\'}"; v="${v%\'}"
            case "$k" in
                ID)          OS_ID="${v,,}" ;;
                ID_LIKE)     OS_ID_LIKE="${v,,}" ;;
                VERSION_ID)  OS_VERSION_ID="$v" ;;
                PRETTY_NAME) OS_PRETTY="$v" ;;
            esac
        done < "$os_release"
    fi

    if [ -e /run/ostree-booted ] \
        || [[ "$OS_ID" =~ ^(steamos|bazzite|silverblue|kinoite|bluefin|aurora|nixos|vanilla|opensuse-microos|aeon|kalpa)$ ]] \
        || [[ "$OS_PRETTY" =~ (Silverblue|Kinoite|Bazzite|SteamOS|Bluefin|Aurora|Atomic|Immutable) ]]; then
        IS_IMMUTABLE=1
    fi
}

os_is_family() {
    local needle="$1"
    [ "$OS_ID" = "$needle" ] && return 0
    case " $OS_ID_LIKE " in
        *" $needle "*) return 0 ;;
    esac
    return 1
}

# Returns 0 if the host APT repositories can actually satisfy Vanta's core
# runtime dependencies (notably libwebkitgtk-6.0-4, which is absent or broken
# on Raspberry Pi OS Bookworm and some Debian/Ubuntu derivatives).
apt_deps_installable() {
    local arch="$1"
    if [ -n "${VANTA_INSTALL_APT_DEPS_OK:-}" ]; then
        [ "$VANTA_INSTALL_APT_DEPS_OK" = "1" ]
        return
    fi

    # Verify dpkg native architecture matches the target .deb architecture.
    if command -v dpkg >/dev/null 2>&1; then
        local dpkg_arch expected_dpkg_arch
        dpkg_arch=$(dpkg --print-architecture 2>/dev/null || true)
        case "$arch" in
            x86_64)  expected_dpkg_arch="amd64" ;;
            aarch64) expected_dpkg_arch="arm64" ;;
            *) return 1 ;;
        esac
        if [ -n "$dpkg_arch" ] && [ "$dpkg_arch" != "$expected_dpkg_arch" ]; then
            return 1
        fi
    fi

    command -v apt-cache >/dev/null 2>&1 || return 1
    local pkg cand
    for pkg in libgtk-4-1 libadwaita-1-0 libwebkitgtk-6.0-4; do
        cand=$(apt-cache policy "$pkg" 2>/dev/null | awk '/Candidate:/ { print $2; exit }')
        if [ -z "$cand" ] || [ "$cand" = "(none)" ]; then
            return 1
        fi
    done

    # Simulate installing the core runtime libraries without touching the system
    # so broken/conflicting transitive deps (e.g. libwebkitgtk on Raspberry Pi OS)
    # are caught before adding the APT repo.
    apt-get -s -o Debug::NoLocking=true install \
        libgtk-4-1 libadwaita-1-0 libwebkitgtk-6.0-4 >/dev/null 2>&1
}

choose_method() {
    local arch="$1"

    if [ "$METHOD" != "auto" ]; then
        printf '%s' "$METHOD"
        return 0
    fi

    # Immutable / atomic desktops: Flatpak first, then AppImage.
    if [ "$IS_IMMUTABLE" -eq 1 ]; then
        if command -v flatpak >/dev/null 2>&1; then
            printf 'flatpak'
        else
            printf 'appimage'
        fi
        return 0
    fi

    # Debian / Ubuntu / Raspberry Pi OS family: signed APT repo only when
    # glibc >= 2.39, core runtime deps (including libwebkitgtk-6.0-4) are
    # installable, and the repo is live.
    if os_is_family "ubuntu" || os_is_family "debian"; then
        if command -v apt-get >/dev/null 2>&1 && glibc_meets_floor && apt_deps_installable "$arch"; then
            if [ "$DRY_RUN" -eq 1 ] || url_ok "${BASE_URL}/apt/dists/stable/InRelease"; then
                printf 'apt'
                return 0
            fi
        fi
    fi

    # Fedora 44: signed DNF repo when live.
    if os_is_family "fedora" && [ "$OS_VERSION_ID" = "44" ] && command -v dnf >/dev/null 2>&1; then
        if [ "$DRY_RUN" -eq 1 ] || url_ok "${BASE_URL}/rpm/44/${arch}/repodata/repomd.xml"; then
            printf 'dnf'
            return 0
        fi
    fi

    # Arch family (x86_64): signed Pacman repo when live.
    if os_is_family "arch" && [ "$arch" = "x86_64" ] && command -v pacman >/dev/null 2>&1; then
        if [ "$DRY_RUN" -eq 1 ] || url_ok "${BASE_URL}/arch/x86_64/vanta.db"; then
            printf 'pacman'
            return 0
        fi
    fi

    # Universal fallback:
    # - Prefer Flatpak when installed, OR on older glibc (< 2.39, e.g. Debian 12 /
    #   Raspberry Pi OS Bookworm) where apt-get can bootstrap flatpak and the
    #   AppImage would fail the glibc 2.39 floor.
    # - Otherwise fall back to the self-contained AppImage (bundles GTK 4 + WebKitGTK 6.0).
    if command -v flatpak >/dev/null 2>&1; then
        printf 'flatpak'
    elif ! glibc_meets_floor && command -v apt-get >/dev/null 2>&1; then
        printf 'flatpak'
    else
        printf 'appimage'
    fi
}

# ---- Installers ------------------------------------------------------------

install_flatpak() {
    if ! command -v flatpak >/dev/null 2>&1; then
        if command -v apt-get >/dev/null 2>&1; then
            info "Flatpak is not installed yet; installing flatpak via apt..."
            as_root apt-get update
            as_root apt-get install -y flatpak
        else
            need_cmd flatpak
        fi
    fi
    info "Installing Vanta via signed Flatpak remote (branch: ${CHANNEL})..."

    # Ensure Flathub exists so GNOME Platform runtime can be pulled automatically.
    run flatpak remote-add --user --if-not-exists flathub \
        https://dl.flathub.org/repo/flathub.flatpakrepo

    run flatpak remote-add --user --if-not-exists vanta \
        "${BASE_URL}/vanta.flatpakrepo"

    # If the user previously added the remote while unsigned, upgrade it to GPG-verified.
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '  %s[dry-run]%s curl -fsSL %q | verify fingerprint %s | flatpak remote-modify --user --gpg-import=- vanta\n' \
            "${C_YELLOW}" "${C_RESET}" "${BASE_URL}/vanta.gpg" "$FLATPAK_GPG_FPR"
    elif url_ok "${BASE_URL}/vanta.gpg"; then
        local keyfile="${TMPDIR_WORK}/vanta-flatpak.gpg"
        curl -fsSL "${BASE_URL}/vanta.gpg" -o "$keyfile"
        verify_gpg_fingerprint "$keyfile" "$FLATPAK_GPG_FPR"
        flatpak remote-modify --user --gpg-import="$keyfile" vanta
    fi

    local install_args=(install --user -y vanta "${APP_ID}//${CHANNEL}")
    run flatpak "${install_args[@]}"

    ok "Vanta installed via Flatpak (${CHANNEL})."
    info "Launch from your application menu or run: flatpak run ${APP_ID}"
}

install_apt() {
    local arch="$1"
    need_cmd apt-get
    if ! glibc_meets_floor; then
        warn "Your system glibc is older than 2.39 (Ubuntu 24.04 floor). The native .deb may not run; consider --method flatpak instead."
    fi

    info "Setting up Vanta signed APT repository..."
    local keyfile="${TMPDIR_WORK}/vanta-archive-keyring.gpg"
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '  %s[dry-run]%s curl -fsSL %q -o %q && verify fingerprint %s\n' \
            "${C_YELLOW}" "${C_RESET}" "${BASE_URL}/apt/vanta-archive-keyring.gpg" "$keyfile" "$APT_GPG_FPR"
    else
        curl -fsSL "${BASE_URL}/apt/vanta-archive-keyring.gpg" -o "$keyfile"
        verify_gpg_fingerprint "$keyfile" "$APT_GPG_FPR"
    fi

    as_root install -m 0755 -d /etc/apt/keyrings
    as_root install -m 0644 "$keyfile" /etc/apt/keyrings/vanta-archive-keyring.gpg

    local list_line="deb [signed-by=/etc/apt/keyrings/vanta-archive-keyring.gpg] ${BASE_URL}/apt stable main"
    local list_tmp="${TMPDIR_WORK}/vanta.list"
    printf '%s\n' "$list_line" > "$list_tmp"
    as_root install -m 0644 "$list_tmp" /etc/apt/sources.list.d/vanta.list

    as_root apt-get update
    if ! as_root apt-get install -y vanta; then
        warn "APT could not satisfy vanta's dependencies (commonly libwebkitgtk-6.0-4 on Raspberry Pi OS or older Debian/Ubuntu)."
        as_root rm -f /etc/apt/sources.list.d/vanta.list
        if [ "$METHOD" = "auto" ]; then
            if command -v flatpak >/dev/null 2>&1 || ! glibc_meets_floor; then
                info "Falling back to signed Flatpak (includes its own GTK 4 + WebKitGTK 6.0 runtime)..."
                install_flatpak
                return 0
            else
                info "Falling back to self-contained AppImage (bundles GTK 4 + WebKitGTK 6.0)..."
                install_appimage "$arch"
                return 0
            fi
        fi
        die "APT install failed due to unmet system dependencies. Re-run with --method flatpak or --method appimage."
    fi

    ok "Vanta installed via signed APT repository."
    info "Launch from your application menu or run: vanta"
}

install_dnf() {
    need_cmd dnf
    if [ "$OS_VERSION_ID" != "44" ]; then
        warn "Vanta's DNF repository currently targets Fedora 44 (detected VERSION_ID=${OS_VERSION_ID:-unknown}). Consider --method flatpak if dnf returns 404."
    fi

    info "Setting up Vanta signed DNF repository..."
    local keyfile="${TMPDIR_WORK}/RPM-GPG-KEY-vanta"
    local repofile="${TMPDIR_WORK}/vanta.repo"
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '  %s[dry-run]%s curl -fsSL %q -o %q && verify fingerprint %s\n' \
            "${C_YELLOW}" "${C_RESET}" "${BASE_URL}/rpm/RPM-GPG-KEY-vanta" "$keyfile" "$RPM_GPG_FPR"
        printf '  %s[dry-run]%s curl -fsSL %q -o %q\n' \
            "${C_YELLOW}" "${C_RESET}" "${BASE_URL}/vanta.repo" "$repofile"
    else
        curl -fsSL "${BASE_URL}/rpm/RPM-GPG-KEY-vanta" -o "$keyfile"
        verify_gpg_fingerprint "$keyfile" "$RPM_GPG_FPR"
        curl -fsSL "${BASE_URL}/vanta.repo" -o "$repofile"
    fi

    as_root install -m 0644 "$repofile" /etc/yum.repos.d/vanta.repo
    as_root dnf install -y vanta

    ok "Vanta installed via signed DNF repository."
    info "Launch from your application menu or run: vanta"
}

install_pacman() {
    local arch="$1"
    need_cmd pacman
    need_cmd pacman-key
    if [ "$arch" != "x86_64" ]; then
        die "Vanta's Pacman repository currently ships x86_64 packages only. Use --method flatpak or --method appimage on ${arch}."
    fi

    info "Setting up Vanta signed Pacman repository..."
    local keyfile="${TMPDIR_WORK}/vanta-archive-keyring.asc"
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '  %s[dry-run]%s curl -fsSL %q -o %q && verify fingerprint %s\n' \
            "${C_YELLOW}" "${C_RESET}" "${BASE_URL}/arch/vanta-archive-keyring.asc" "$keyfile" "$ARCH_GPG_FPR"
    else
        curl -fsSL "${BASE_URL}/arch/vanta-archive-keyring.asc" -o "$keyfile"
        verify_gpg_fingerprint "$keyfile" "$ARCH_GPG_FPR"
    fi

    as_root pacman-key --add "$keyfile"
    as_root pacman-key --lsign-key "$ARCH_GPG_FPR"

    if grep -qE '^[[:space:]]*\[vanta\][[:space:]]*$' /etc/pacman.conf 2>/dev/null; then
        info "[vanta] repository is already configured in /etc/pacman.conf."
    else
        local conf_tmp="${TMPDIR_WORK}/pacman.conf.new"
        if [ "$DRY_RUN" -eq 1 ]; then
            printf '  %s[dry-run]%s append [vanta] repository stanza to /etc/pacman.conf\n' "${C_YELLOW}" "${C_RESET}"
        else
            cp /etc/pacman.conf "$conf_tmp"
            cat >> "$conf_tmp" <<EOF

[vanta]
Server = ${BASE_URL}/arch/\$arch
SigLevel = DatabaseRequired PackageNever
EOF
            as_root install -m 0644 "$conf_tmp" /etc/pacman.conf
        fi
    fi

    as_root pacman -Sy --needed --noconfirm vanta

    ok "Vanta installed via signed Pacman repository."
    info "Launch from your application menu or run: vanta"
}

install_appimage() {
    local arch="$1"
    info "Installing latest Vanta AppImage (${arch})..."

    local install_bin_dir="${HOME}/.local/bin"
    local install_app_dir="${HOME}/.local/share/applications"
    local install_icon_dir="${HOME}/.local/share/icons/hicolor/scalable/apps"
    local target_appimage="${install_bin_dir}/Vanta.AppImage"
    local target_link="${install_bin_dir}/vanta"
    local target_desktop="${install_app_dir}/${APP_ID}.desktop"
    local target_icon="${install_icon_dir}/${APP_ID}.svg"

    local download_url=""
    if [ "$DRY_RUN" -eq 1 ]; then
        download_url="https://github.com/gpt4omni/vanta/releases/latest/download/Vanta-<latest>-${arch}.AppImage"
    else
        local json
        json=$(curl -fsSL "$REPO_API")
        download_url=$(printf '%s' "$json" \
            | grep -oE '"browser_download_url":[[:space:]]*"[^"]+-'"${arch}"'\.AppImage"' \
            | head -n1 \
            | sed -E 's/.*"([^"]+)"$/\1/')
        [ -n "$download_url" ] || die "Could not find a ${arch} AppImage asset in the latest GitHub release."
    fi

    run mkdir -p "$install_bin_dir" "$install_app_dir" "$install_icon_dir"

    local dl_tmp="${TMPDIR_WORK}/Vanta.AppImage"
    info "Downloading ${download_url}..."
    run curl -fL --progress-bar "$download_url" -o "$dl_tmp"
    run install -m 0755 "$dl_tmp" "$target_appimage"

    # Install a thin launcher at ~/.local/bin/vanta that automatically sets
    # APPIMAGE_EXTRACT_AND_RUN=1 when libfuse.so.2 is absent (e.g. Raspberry Pi OS
    # and Ubuntu 24.04+), so AppImage launches work without libfuse2.
    if [ "$DRY_RUN" -eq 1 ]; then
        printf '  %s[dry-run]%s write launcher wrapper %q -> %q\n' "${C_YELLOW}" "${C_RESET}" "$target_link" "$target_appimage"
    else
        rm -f "$target_link"
        cat > "$target_link" <<EOF
#!/usr/bin/env bash
set -euo pipefail
if [ -z "\${APPIMAGE_EXTRACT_AND_RUN:-}" ]; then
    if ! { command -v ldconfig >/dev/null 2>&1 && ldconfig -p 2>/dev/null | grep -q 'libfuse\.so\.2'; } \
        && [ ! -e /usr/lib/libfuse.so.2 ] \
        && [ ! -e /usr/lib/x86_64-linux-gnu/libfuse.so.2 ] \
        && [ ! -e /usr/lib/aarch64-linux-gnu/libfuse.so.2 ]; then
        export APPIMAGE_EXTRACT_AND_RUN=1
    fi
fi
exec "${target_appimage}" "\$@"
EOF
        chmod 0755 "$target_link"
    fi

    if [ "$DRY_RUN" -eq 1 ]; then
        printf '  %s[dry-run]%s download icon to %q\n' "${C_YELLOW}" "${C_RESET}" "$target_icon"
        printf '  %s[dry-run]%s write desktop entry %q\n' "${C_YELLOW}" "${C_RESET}" "$target_desktop"
    else
        curl -fsSL "$ICON_URL" -o "$target_icon" 2>/dev/null || true
        cat > "$target_desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Vanta
GenericName=Roblox Client
Comment=Run Roblox natively on Linux
Exec=${target_link} %u
Icon=${APP_ID}
Terminal=false
Categories=Game;
MimeType=x-scheme-handler/roblox-player;x-scheme-handler/roblox;
StartupNotify=true
StartupWMClass=Vanta
EOF
        if command -v update-desktop-database >/dev/null 2>&1; then
            update-desktop-database "$install_app_dir" 2>/dev/null || true
        fi
        if command -v xdg-mime >/dev/null 2>&1; then
            xdg-mime default "${APP_ID}.desktop" x-scheme-handler/roblox-player 2>/dev/null || true
            xdg-mime default "${APP_ID}.desktop" x-scheme-handler/roblox 2>/dev/null || true
        fi
    fi

    ok "Vanta AppImage installed to ${target_appimage}."
    info "Desktop entry and roblox:// link handler configured."
    case ":${PATH}:" in
        *":${install_bin_dir}:"*) info "Launch from your application menu or run: vanta" ;;
        *) info "Launch from your application menu or run: ${target_link} (add ~/.local/bin to PATH for 'vanta')" ;;
    esac
}

# ---- Main ------------------------------------------------------------------

main() {
    while [ $# -gt 0 ]; do
        case "$1" in
            --method)
                [ $# -ge 2 ] || die "--method requires an argument"
                METHOD="$2"
                shift 2
                ;;
            --method=*)
                METHOD="${1#*=}"
                shift
                ;;
            --channel)
                [ $# -ge 2 ] || die "--channel requires an argument"
                CHANNEL="$2"
                shift 2
                ;;
            --channel=*)
                CHANNEL="${1#*=}"
                shift
                ;;
            -y|--yes)
                ASSUME_YES=1
                shift
                ;;
            --dry-run)
                DRY_RUN=1
                shift
                ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                die "Unknown option: $1 (run with --help for usage)"
                ;;
        esac
    done

    case "$METHOD" in
        auto|flatpak|apt|dnf|pacman|appimage) ;;
        *) die "Invalid --method '${METHOD}'. Choose: auto, flatpak, apt, dnf, pacman, appimage." ;;
    esac

    case "$CHANNEL" in
        stable|master) ;;
        *) die "Invalid --channel '${CHANNEL}'. Choose: stable or master." ;;
    esac

    [ "$(uname -s)" = "Linux" ] || die "Vanta runs on Linux only (detected $(uname -s))."
    need_cmd curl

    TMPDIR_WORK=$(mktemp -d)
    chmod 700 "$TMPDIR_WORK"

    local arch chosen
    arch=$(detect_arch)
    detect_os
    chosen=$(choose_method "$arch")

    info "Detected: ${OS_PRETTY} (${arch})"
    if [ "$IS_IMMUTABLE" -eq 1 ]; then
        info "Immutable/atomic desktop detected; preferring sandboxed install."
    fi
    info "Selected install method: ${C_BOLD}${chosen}${C_RESET}"

    if [ "$DRY_RUN" -eq 0 ] && [ "$ASSUME_YES" -eq 0 ] && [ -t 0 ]; then
        printf '%s[vanta]%s Proceed with %s install? [Y/n] ' "${C_CYAN}${C_BOLD}" "${C_RESET}" "$chosen"
        local reply=""
        read -r reply || true
        case "$reply" in
            ""|[Yy]|[Yy][Ee][Ss]) ;;
            *) info "Cancelled."; exit 0 ;;
        esac
    fi

    case "$chosen" in
        flatpak)  install_flatpak ;;
        apt)      install_apt "$arch" ;;
        dnf)      install_dnf ;;
        pacman)   install_pacman "$arch" ;;
        appimage) install_appimage "$arch" ;;
    esac

    info "Need help or hit an issue? Run 'vanta --doctor' or join https://discord.gg/Jt5MW7UBsC"
}

main "$@"
