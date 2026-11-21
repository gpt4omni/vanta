<div align="center">

<img src="https://raw.githubusercontent.com/gpt4omni/vanta/main/packaging/banner.svg" alt="Vanta" width="520">

### Run Roblox natively on Linux — fast, open, and unmodified.

Runs Roblox's official Android client directly on **x86-64** and **aarch64** Linux with no emulator, container, or virtual machine.

<p>
  <a href="https://github.com/gpt4omni/vanta/releases/latest">
    <img src="https://img.shields.io/github/v/release/gpt4omni/vanta?style=for-the-badge&color=18181B&label=Release" alt="Latest Release">
  </a>
  <a href="https://vanta.mintlify.app">
    <img src="https://img.shields.io/badge/Docs-vanta.mintlify.app-27272A?style=for-the-badge&logo=readthedocs&logoColor=white" alt="Documentation">
  </a>
  <a href="https://discord.gg/qJzU3Xfr9b">
    <img src="https://img.shields.io/badge/Discord-Join%20Community-5865F2?style=for-the-badge&logo=discord&logoColor=white" alt="Join the Vanta Discord">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/License-GPL--3.0--or--later-3F3F46?style=for-the-badge" alt="GPL-3.0-or-later">
  </a>
</p>

<p>
  <a href="#install">Install</a> &bull;
  <a href="#highlights">Highlights</a> &bull;
  <a href="#demo">Demo</a> &bull;
  <a href="#shortcuts--tools">Shortcuts &amp; Tools</a> &bull;
  <a href="#how-it-works">How It Works</a> &bull;
  <a href="https://vanta.mintlify.app">Full Docs</a>
</p>

</div>

---

## Highlights

| | Feature | What it does |
| :---: | :--- | :--- |
| **🚀** | **Built for High FPS & Smooth 1% Lows** | GPU compute ETC2/EAC → native hardware BC1–BC5 transcoding on NVIDIA & non-ETC2 GPUs, 2 MiB huge pages for `libroblox.so`, adaptive mutexes, advisory `MADV_FREE` TLB-shootdown filtering, 4-image swapchain ring, Wayland `wp_tearing_control_v1`, `libjnivm` Rocket hooks, and a singleflight positive/negative DNS cache. |
| **📊** | **Built-in Live Performance HUD (`F8`)** | Toggleable real-time **FPS, 1% lows, 0.1% lows, avg & P99 frametime** powered directly by Vanta's lock-free present ring buffer — no external overlay setup required. |
| **🎨** | **Non-Destructive Asset Modding Manager** | Overlay custom fonts, cursors, skyboxes, textures, or classic audio from `<profile>/mods/` directly over `base.apk` without ever modifying the APK — survives every Roblox update, with ZIP/Folder import and per-pack toggles in **Settings → Mods**. |
| **🌍** | **Local Server Region & Quick Rejoin** | Detects your active game server IP/Job ID from the log and resolves Roblox AS22697 datacenters **100% locally** (zero external tracking), with a headerbar pill for **Copy Invite Link**, **Copy Server Info**, **Quick Rejoin**, and optional join notifications. |
| **🎬** | **Native Recording (`F9`), VR & Multi-Profile** | Built-in `ffmpeg` game + audio recording (`F9`), isolated multi-account profiles with desktop keyring storage, **Meta Quest VR** over OpenXR (WiVRn), and hot-swappable **Deno plugins** with default-deny capabilities. |
| **🛡️** | **Strict Zero-Hooking Architecture** | Fixes compatibility and performance strictly at the Android/Vulkan/Wayland/Bionic framework boundary — **never** patches or hooks `libroblox.so` ([ADR-001](docs/adr/ADR-001-in-process-hooking.md)). |

---

## Demo

<div align="center">
<table>
  <tr>
    <td align="center" width="58%">
      <img src="https://raw.githubusercontent.com/gpt4omni/vanta/main/docs/media/vanta-doors.gif" alt="Roblox DOORS running under Vanta on Linux" width="100%">
      <br>
      <sub><b>Roblox DOORS</b> running natively on Linux (<a href="https://raw.githubusercontent.com/gpt4omni/vanta/main/docs/media/vanta-doors.mp4">full video</a> &bull; <a href="docs/media">more media</a>)</sub>
    </td>
    <td align="center" width="42%">
      <img src="https://raw.githubusercontent.com/gpt4omni/vanta/main/docs/media/vanta-vr.webp" alt="Roblox Meta Quest build in VR on Linux, playing TUNNELER" width="100%">
      <br>
      <sub><b>Meta Quest VR</b> playing <a href="https://www.roblox.com/games/4635669637/TUNNELER">TUNNELER</a> (<a href="https://raw.githubusercontent.com/gpt4omni/vanta/main/docs/media/vanta-vr.mp4">full video</a> &bull; <a href="docs/vr.md">VR guide</a>)</sub>
    </td>
  </tr>
</table>
</div>

---

## Install

> **Note:** Vanta does not ship Roblox. On first launch, click **Download Roblox** to fetch the official Android APK (verified against Roblox's own signing certificate), or import an existing copy from [Sober](https://sober.vinegarhq.org/). Wayland is the primary backend; X11 is also supported ([ADR-024](docs/adr/ADR-024-x11-is-supported-again.md)).

### Flatpak (Recommended)

Runs on a consistent GNOME runtime with the exact GTK4, WebKitGTK, Vulkan loader, and audio libraries Vanta is built against:

```bash
flatpak remote-add --if-not-exists vanta https://gpt4omni.github.io/vanta/vanta.flatpakrepo
flatpak install vanta io.github.gpt4omni.Vanta
flatpak run io.github.gpt4omni.Vanta
```

Want every commit from `main`? Install the `master` branch instead:

```bash
flatpak install vanta io.github.gpt4omni.Vanta//master
```

<details>
<summary><b>AppImage, Native Packages (.deb / .rpm / Arch), and Building from Source</b></summary>

#### AppImage ([Releases Page](https://github.com/gpt4omni/vanta/releases))

```bash
chmod +x Vanta-*.AppImage && ./Vanta-*.AppImage   # -x86_64 or -aarch64
```

#### Native Packages (each signed with keyless Sigstore / cosign)

```bash
sudo apt install ./vanta_*_amd64.deb      # or _arm64.deb
sudo dnf install ./vanta-*.x86_64.rpm     # or .aarch64.rpm (Fedora 44)
sudo pacman -U vanta-*-x86_64.pkg.tar.zst # x86-64 only
```

Signed `apt`, `dnf`, and `pacman` repository setup commands are in [`docs/install.md`](docs/install.md).

#### Build from Source

Requires **Clang** (AOSP bionic uses C11 `_Atomic` in C++ headers, which GCC rejects), **GTK4 ≥ 4.12**, and **libadwaita ≥ 1.5** dev headers (`pipewire-devel` and `webkitgtk6.0-devel` optional; add `--features vanta-runtime/vr` + Boost headers for VR):

```bash
git clone --recursive https://github.com/gpt4omni/vanta
cd vanta
cargo build --release
```

Full dependency list & Nix flake notes: [`docs/install.md`](docs/install.md#from-source) & [`CONTRIBUTING.md`](CONTRIBUTING.md).

</details>

---

## Shortcuts & Tools

| Shortcut / Command | Description |
| :--- | :--- |
| **`F8`** | Toggle the live **Performance HUD** (FPS, 1% low, 0.1% low, avg & P99 frametime) |
| **`F9`** | Start / stop built-in **`ffmpeg` game recording** (with optional game audio) |
| **Settings → Mods** | Manage non-destructive `<profile>/mods/` packs (import ZIP/folder, toggle packs) |
| **Settings → Graphics** | Present mode (`Mailbox` / `Immediate` / `FIFO`), FPS limit (`Display` / `90–480` / `Uncapped`), `vkBasalt` shaders, `MangoHUD` |
| **Settings → FastFlags** | Edit per-profile `flags.json` or import Bloxstrap / Fishstrap / Sober flag lists (`vanta --import-flags`) |
| **Settings → Plugins** | Install & hot-swap sandboxed Deno plugins (`.tar.zst`, per-profile capability grants) |
| **`vanta --doctor`** | Diagnose display, Vulkan, audio, keyring, and Roblox build issues |
| **`vanta --profile P1 --play`** | Launch isolated multi-account instances side by side ([PartyDeck / gamescope guide](docs/partydeck.md)) |

Full configuration & environment-variable reference: **[vanta.mintlify.app](https://vanta.mintlify.app)** · [`docs/`](docs/README.md) · [`docs/fastflags.md`](docs/fastflags.md).

---

## How It Works

Vanta maps and relocates Roblox's unmodified `libroblox.so` using a ported AOSP bionic linker rather than the host dynamic linker:

```text
libroblox.so (unmodified)
   ├── ELF symbols ──► vanta (Android NDK / Vulkan / AAudio / bionic shims)
   │               ├── host  (forwarded directly to glibc)
   │               └── stub  (honest failure reporting, never silent fakes)
   └── JNI calls   ──► libjnivm + Rocket fast-path hooks ──► Wayland / X11 / GTK4 / PipeWire
```

- **Dynamic ELF symbol discovery:** Reads `libroblox.so`'s own imports at load time rather than maintaining a fragile hardcoded table ([ADR-034](docs/adr/ADR-034-symbol-resolution-asks-the-library.md)).
- **No binary patching or in-process hooking:** Every compatibility fix and optimization stays strictly at the platform/loader/Vulkan/FastFlag boundary ([ADR-001](docs/adr/ADR-001-in-process-hooking.md)).
- Full architecture diagram and data flow: [`docs/architecture.md`](docs/architecture.md).

---

## Status & Known Issues

**Experimental.** Full feature matrix in [`docs/status.md`](docs/status.md).

- **Working:** Joining public & private servers, sign-in, keyboard/mouse & pointer capture, IME text entry, PipeWire/AAudio sound & voice chat, fullscreen & Wayland tearing, multi-account profiles, built-in recording (`F9`), Performance HUD (`F8`), Asset Mod Manager, Server Region & Quick Rejoin, FastFlags, plugins, and experimental Meta Quest VR.
- **Known issues:**
  - Roblox can stop drawing on the loading screen after sign-in — launcher auto-restarts up to twice ([#92](https://github.com/gpt4omni/vanta/issues/92)).
  - Window creation issues on some COSMIC, KWin, and wlroots setups ([#38](https://github.com/gpt4omni/vanta/issues/38)).
  - Controllers work on common pads, though button glyphs may not match pad brand and rumble/force feedback is unsupported ([`docs/controllers.md`](docs/controllers.md)).
  - `aarch64` builds pass CI on real ARM64 hardware, but live in-game ARM64 testing is still early ([`docs/multiarch.md`](docs/multiarch.md)).

---

## What Vanta Is Not

- **Not affiliated with or endorsed by Roblox Corporation.** Roblox does not support third-party clients and has banned accounts in waves (including false positives). Do not risk an account you cannot afford to lose.
- **Does not ship Roblox's client.** You supply or fetch the official signature-verified Android APK yourself.
- **Not a cheat, exploit, or script/memory injector.** There is no script execution, hooking, or memory access into Roblox — absent from the API by design ([ADR-001](docs/adr/ADR-001-in-process-hooking.md), [ADR-003](docs/adr/ADR-003-plugin-isolation.md)). Plugins extend Vanta, not Roblox.

---

## Credits, Origins & License

- **Fork of [Cordial](https://github.com/luohoa97/cordial):** Vanta uses the `vanta` binary, XDG config/data paths, and `VANTA_` environment variables (existing Cordial profiles are not migrated automatically; historical records keep upstream names).
- **Credit to [Sober](https://sober.vinegarhq.org/):** Sober proved native Android Roblox runtimes on Linux are possible. Sober's code is closed and was never read; only its public issue tracker was used as a reference ([ADR-017](docs/adr/ADR-017-sober-issue-corpus.md)).
- **AI disclosure:** Implementation leans heavily on AI coding agents; architectural decisions (including reversed ones) are documented openly in [`docs/adr/`](docs/adr).
- **Contributing:** See [`CONTRIBUTING.md`](CONTRIBUTING.md). File bugs and feature requests on [GitHub Issues](https://github.com/gpt4omni/vanta/issues/new/choose) (include `vanta --diagnostics`), security reports via [private advisory](https://github.com/gpt4omni/vanta/security/advisories/new), and general questions on [Discord](https://discord.gg/qJzU3Xfr9b).
- **License:** **GPL-3.0-or-later** ([`LICENSE`](LICENSE)), except [`crates/vanta-protocol`](crates/vanta-protocol) which is dual-licensed **`MIT OR Apache-2.0`**. Third-party notices live in [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).
