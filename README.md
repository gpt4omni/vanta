<div align="center">

<img src="packaging/banner.svg" alt="Vanta" width="520">

### Run Roblox natively on Linux.

Vanta runs Roblox's official, unmodified Android client directly on **x86-64** and **ARM64 (`aarch64`)** Linux. No emulator, container, or virtual machine.

<p>
  <a href="https://github.com/gpt4omni/vanta/releases/latest">
    <img src="https://img.shields.io/github/v/release/gpt4omni/vanta?style=for-the-badge&color=18181B&label=Release" alt="Latest Release">
  </a>
  <a href="docs/README.md">
    <img src="https://img.shields.io/badge/Docs-Read%20Docs-27272A?style=for-the-badge&logo=readthedocs&logoColor=white" alt="Documentation">
  </a>
  <a href="https://discord.gg/Jt5MW7UBsC">
    <img src="https://img.shields.io/badge/Discord-Join%20Community-5865F2?style=for-the-badge&logo=discord&logoColor=white" alt="Join the Vanta Discord">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/License-GPL--3.0--or--later-3F3F46?style=for-the-badge" alt="GPL-3.0-or-later">
  </a>
</p>

<p>
  <a href="#install">Install</a> &bull;
  <a href="#features">Features</a> &bull;
  <a href="#demo">Demo</a> &bull;
  <a href="#shortcuts--commands">Shortcuts</a> &bull;
  <a href="#notes--disclaimer">Notes</a> &bull;
  <a href="docs/README.md">Docs</a>
</p>

</div>

---

## Install

> **First launch:** Vanta does not include Roblox. When you open Vanta, click **Download Roblox** to fetch the official Android APK (verified against Roblox's signing certificate), or import an existing copy from [Sober](https://sober.vinegarhq.org/).

### Flatpak (Recommended, GPG-Signed)

Works on most Linux distros (x86-64 & ARM64) with automatic updates and the exact GTK4, WebKitGTK, Vulkan, and audio libraries Vanta is tested against:

```bash
flatpak remote-add --if-not-exists vanta https://gpt4omni.github.io/vanta/vanta.flatpakrepo
flatpak install vanta io.github.gpt4omni.Vanta
flatpak run io.github.gpt4omni.Vanta
```

To test the latest commits from `main` instead of tagged releases, install `io.github.gpt4omni.Vanta//master`.

### Native Signed Repositories (Automatic Updates)

<details>
<summary><b>Debian / Ubuntu (APT, x86-64 & ARM64)</b></summary>

```bash
sudo install -m 0755 -d /etc/apt/keyrings
sudo curl -fsSL https://gpt4omni.github.io/vanta/apt/vanta-archive-keyring.gpg \
    -o /etc/apt/keyrings/vanta-archive-keyring.gpg
echo "deb [signed-by=/etc/apt/keyrings/vanta-archive-keyring.gpg] https://gpt4omni.github.io/vanta/apt stable main" \
    | sudo tee /etc/apt/sources.list.d/vanta.list
sudo apt update && sudo apt install vanta
```

</details>

<details>
<summary><b>Fedora 44 (DNF, x86-64 & ARM64)</b></summary>

```bash
sudo curl -fsSL https://gpt4omni.github.io/vanta/vanta.repo \
    -o /etc/yum.repos.d/vanta.repo
sudo dnf install vanta
```

</details>

<details>
<summary><b>Arch Linux (Pacman, x86-64)</b></summary>

```bash
curl -fsSL https://gpt4omni.github.io/vanta/arch/vanta-archive-keyring.asc \
    | sudo pacman-key --add -
sudo pacman-key --lsign-key A75EDC4B9B7383BB2FF0F6BEB5018BF3AB2ACA11
```

Then add this to `/etc/pacman.conf` and run `sudo pacman -Syu vanta`:

```ini
[vanta]
Server = https://gpt4omni.github.io/vanta/arch/$arch
SigLevel = DatabaseRequired PackageNever
```

</details>

### AppImage & Direct Downloads (x86-64 & ARM64)

Download `Vanta-*-x86_64.AppImage`, `Vanta-*-aarch64.AppImage`, `.deb`, `.rpm`, or `.pkg.tar.zst` (all signed with Sigstore/cosign) from [GitHub Releases](https://github.com/gpt4omni/vanta/releases/latest):

```bash
chmod +x Vanta-*.AppImage
./Vanta-*.AppImage
```

Key fingerprints, cosign verification, and Nix flake setup are in [`docs/install.md`](docs/install.md).

<details>
<summary><b>Build from Source</b></summary>

Requires **Clang** (AOSP bionic does not compile with GCC), **GTK4 (>= 4.12)**, and **libadwaita (>= 1.5)** development headers. `pipewire` and `webkitgtk6.0` headers are optional; add `--features vanta-runtime/vr` and Boost headers for VR support.

```bash
git clone --recursive https://github.com/gpt4omni/vanta
cd vanta
cargo build --release
```

Full dependency list and Nix flake instructions are in [`docs/install.md`](docs/install.md#from-source) and [`CONTRIBUTING.md`](CONTRIBUTING.md).

</details>

---

## Features

- **Native Linux performance on x86-64 and ARM64 (`aarch64`):** Runs Roblox's official Android client directly with no emulator or VM. Works on Wayland and X11, including tiling compositors like Hyprland and Niri.
- **Performance HUD (`F8`):** Press `F8` anytime to see live FPS, 1% lows, and frametime in the title bar.
- **Built-in Recording (`F9`):** Record gameplay with game audio using `ffmpeg` without setting up an external recorder ([recording guide](docs/recording.md)).
- **Asset Mods:** Load custom fonts, cursors, sounds, skyboxes, and textures from **Settings > Mods** without modifying Roblox's APK, so your mods survive Roblox updates ([asset mod guide](docs/asset-overrides.md)).
- **Server Region & Quick Rejoin:** See your current server region right in the title bar, copy an invite link, or rejoin the same server in one click.
- **Multi-Account Profiles, FastFlags & Plugins:** Run multiple accounts side by side ([PartyDeck / gamescope guide](docs/partydeck.md)), edit or import [FastFlags](docs/fastflags.md), and install sandboxed [plugins](docs/plugins.md).
- **Meta Quest VR:** Play Roblox in VR on Linux over OpenXR / WiVRn ([VR guide](docs/vr.md)).

---

## Demo

<div align="center">
<table>
  <tr>
    <td align="center" width="58%">
      <img src="docs/media/vanta-doors.gif" alt="Roblox DOORS running under Vanta on Linux" width="100%">
      <br>
      <sub><b>Roblox DOORS</b> running natively on Linux (<a href="docs/media/vanta-doors.mp4">full video</a> &bull; <a href="docs/media">more media</a>)</sub>
    </td>
    <td align="center" width="42%">
      <img src="docs/media/vanta-vr.webp" alt="Roblox Meta Quest build in VR on Linux, playing TUNNELER" width="100%">
      <br>
      <sub><b>Meta Quest VR</b> playing <a href="https://www.roblox.com/games/4635669637/TUNNELER">TUNNELER</a> (<a href="docs/media/vanta-vr.mp4">full video</a> &bull; <a href="docs/vr.md">VR guide</a>)</sub>
    </td>
  </tr>
</table>
</div>

---

## Shortcuts & Commands

| Shortcut / Command | What it does |
| :--- | :--- |
| **`F8`** | Toggle the live Performance HUD (FPS, 1% lows, frametime) |
| **`F9`** | Start or stop built-in game recording |
| **`vanta --doctor`** | Check your display, Vulkan, audio, keyring, and Roblox setup |
| **`vanta --profile P1 --play`** | Launch a specific account profile directly |
| **`vanta --import-flags <file>`** | Import FastFlags from Bloxstrap, Fishstrap, or Sober |

Looking for architecture notes, troubleshooting, or full feature status? See [`docs/`](docs/README.md) and [`docs/status.md`](docs/status.md).

---

## Notes & Disclaimer

- **Not affiliated with Roblox Corporation.** Roblox does not officially support third-party Linux runtimes and runs automated ban waves. Do not risk an account you cannot afford to lose.
- **Does not ship Roblox.** Vanta contains no Roblox code or assets; you fetch or import the official signature-verified Android APK yourself.
- **Not a cheat or injector.** Vanta never patches or hooks `libroblox.so`, and has no script execution or memory-access APIs. Plugins extend Vanta, not Roblox.
- **Work in progress.** Wayland is the primary backend and X11 is also supported. Common controllers work, though button prompts may differ and rumble is not yet supported ([`docs/controllers.md`](docs/controllers.md)). ARM64 (`aarch64`) support is available and still maturing ([`docs/multiarch.md`](docs/multiarch.md)).
- **Credits:** Forked from [Cordial](https://github.com/luohoa97/cordial). Credit to [Sober](https://sober.vinegarhq.org/) for proving native Android Roblox on Linux works (Sober's closed source was never read; only public issue reports were referenced). Development also makes heavy use of AI coding agents, with design decisions recorded openly in [`docs/`](docs/README.md).
- **Community & Support:** Join our [Discord](https://discord.gg/Jt5MW7UBsC) for help and discussion, open a bug report on [GitHub Issues](https://github.com/gpt4omni/vanta/issues/new/choose) (attach `vanta --diagnostics`), or report security issues privately via [GitHub Security Advisories](https://github.com/gpt4omni/vanta/security/advisories/new).
- **License & Contributing:** Licensed under **GPL-3.0-or-later** ([`LICENSE`](LICENSE)), except [`crates/vanta-protocol`](crates/vanta-protocol) which is dual-licensed **`MIT OR Apache-2.0`**. Third-party attributions are in [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md), and contributor guidelines are in [`CONTRIBUTING.md`](CONTRIBUTING.md).
