---
title: "Installing Vanta"
description: "Install Vanta from a Flatpak, AppImage, deb, rpm, Arch package, Nix or source, and get Roblox's Android build into it."
icon: "download"
---
### Quick Install (Auto-Detect)

Run one command to detect your distro, architecture (`x86_64` or `aarch64`), and
immutable/atomic desktop status, verify Vanta's pinned signing key, and install
through the best signed channel (Flatpak, APT, DNF, Pacman, or AppImage):

```bash
curl -fsSL https://gpt4omni.github.io/vanta/install.sh | bash
```

You can preview what it will do with `--dry-run` or force a channel with
`--method`:

```bash
curl -fsSL https://gpt4omni.github.io/vanta/install.sh | bash -s -- --dry-run
curl -fsSL https://gpt4omni.github.io/vanta/install.sh | bash -s -- --method flatpak
```

**Use the Flatpak unless you have a reason not to.** It is sandboxed, updates in
place, and runs on one fixed runtime (GNOME 50), so the GTK, WebKit, Vulkan and
audio libraries are the ones Vanta is tested against. Most compatibility
problems reported with the other formats come from those libraries differing.

## What you need

- **x86-64 Linux, or aarch64.** Nothing has been run on real ARM64 hardware;
  [`multiarch.md`](/multiarch) says what has been checked.
- **A Wayland session.** Wayland is the primary backend. X11 works too
  ([ADR-024](/adr/ADR-024-x11-is-supported-again)) and is the rougher one:
  what you type into a text box is drawn by a small window laid over the box
  ([ADR-062](/adr/ADR-062-the-x11-editor-is-an-override-redirect-window)),
  which has been tried under openbox, xfwm4, icewm and Xwayland, and not under
  KWin, Mutter's X11 mode or a tiling window manager.
- **Roblox's official Android client, which you supply.** Vanta ships no
  Roblox code, APK or assets.

### Getting Roblox's build

On first run, press **Download Roblox**. Vanta fetches the newest build from a
mirror, checks that Roblox's own signing certificate signed it, and keeps its own
copy in `~/.local/share/vanta/builds/` (in the Flatpak,
`~/.var/app/io.github.gpt4omni.Vanta/data/vanta/builds/`). Why:
[ADR-015](/adr/ADR-015-fetching-the-roblox-build),
[ADR-025](/adr/ADR-025-fetching-from-a-third-party-mirror),
[ADR-054](/adr/ADR-054-vanta-owns-its-roblox-builds).

- **Sober is installed:** the first-run screen also offers **Copy Sober's
  build**, and **Settings → Roblox** has an **Import from Sober** row while Sober
  holds a build Vanta does not. Either copies the files into Vanta's own
  store after the same signature check. Vanta does not change Sober's files
  and does not follow its updates: from then on the build changes when you
  update it in Vanta (**Settings → Updates**), not when Sober updates.
- **You have your own APK:** **Settings → Roblox → Import from a file** copies
  it into the store the same way. Keep `base.apk` and `split_config.x86_64.apk`
  in one folder: on a split build the engine is in the split, and Vanta says
  which file it looked for if it finds none. In the Flatpak the file chooser may
  hand Vanta only the file you picked, so a split build's other half would not
  be seen (INFERRED, not tried): import a single combined APK there, or copy
  Sober's build. To run one APK for a single launch
  without filing it, start Vanta with `VANTA_APK=/path/to/base.apk`.
- **Upgrading from an older Vanta:** the first launch files whatever the old
  layout ran (Vanta's own download, Sober's copy, or an APK chosen in Settings)
  into the store, once. An APK chosen in Settings is imported, every profile with
  no pinned version is pinned to it so it keeps running that file, and the saved
  path is cleared.

Each profile runs the newest build in the store (**Latest**) or one version you
pinned on **Settings → Version**. The store keeps the newest build, the one
before it, any build a profile is pinned to and any a client is running; an
update removes the rest. **Settings → Roblox** lists what is kept, where each
build came from, how much disk it takes and which profiles use it, and removes a
build that nothing needs.

The engine and the game's assets are unpacked next to the build they came from,
and there is nothing to configure for either.

Nothing else is needed to run a package. The Flatpak carries its own toolchain
and libraries.

## Install

Every file on a release page can be verified before you install it; see
[Check what you downloaded](#check-what-you-downloaded).

<Tabs>
<Tab title="Flatpak">


Recommended.

```bash
flatpak remote-add --if-not-exists vanta \
    https://gpt4omni.github.io/vanta/vanta.flatpakrepo
flatpak install vanta io.github.gpt4omni.Vanta
```

Launch it from your application list, or with
`flatpak run io.github.gpt4omni.Vanta`.

| To | Run |
|---|---|
| Update | `flatpak update` |
| Uninstall | `flatpak uninstall io.github.gpt4omni.Vanta` |
| Uninstall and delete profiles, sign-in and the Roblox builds | `flatpak uninstall --delete-data io.github.gpt4omni.Vanta` |

**Branches.** The install above is `stable`, which moves only on a tagged
release. `master` moves on every commit to `main`. The remote once published
only `master`, so an install from before `stable` existed is still on it. To
move it (add `--user` to both if that is how you installed):

```bash
flatpak uninstall io.github.gpt4omni.Vanta//master
flatpak install vanta io.github.gpt4omni.Vanta//stable
```

To follow `main` on purpose, install `io.github.gpt4omni.Vanta//master`.

**Flatpak limitation.** The updater asks NetworkManager on the system bus
whether your connection is metered. The sandbox has no system bus, so the check
fails closed and a Flatpak install treats every connection as metered. It will
not download a Roblox build in the background unless you turn on *Download on
metered connections*. Manual downloads are unaffected.

The [Flatpak workflow](https://github.com/gpt4omni/vanta/actions/workflows/flatpak.yml)
publishes only on a green run, so a red run on `main` means the remote is still
serving the previous build.


</Tab>
<Tab title="AppImage">


One file, any distribution, nothing installed system-wide. Download
`Vanta-<version>-<commit>-x86_64.AppImage` (or `-aarch64`) from the
[releases page](https://github.com/gpt4omni/vanta/releases):

```bash
chmod +x Vanta-*.AppImage
./Vanta-*.AppImage
```

It carries GTK4, libadwaita and WebKitGTK. Delete the file to remove it;
profiles stay in `~/.local/share` until you delete them. It needs FUSE; if it
refuses to start, run it with `--appimage-extract-and-run` to unpack to a
temporary directory instead. Updates are manual.

**The web view is not established on a real machine.** The sign-in window uses
WebKitGTK, which finds its helper processes through paths fixed when it was
built, and the AppImage works around that inside a private mount namespace. That
needs `bwrap` and unprivileged overlay mounts; if your kernel refuses either, the
AppImage says so on standard error and the web view then needs WebKitGTK 6.0
installed at Fedora's path. This has been measured on a stand-in for a machine
with no WebKitGTK, and only for Fedora. If the sign-in window is blank, report it
with whatever the terminal printed. The Flatpak is unaffected. Background:
[ADR-032](/adr/ADR-032-appimage-build-base-moves-to-ubuntu-24-04).


</Tab>
<Tab title="Debian / Ubuntu">


The `.deb` on the [release page](https://github.com/gpt4omni/vanta/releases/latest)
installs with no repository:

```bash
sudo apt install ./vanta_*_amd64.deb     # or _arm64.deb
```

Vanta's own apt repository keeps you current through `apt upgrade`. It is
Vanta's repository, not a package in Debian or Ubuntu
([design note](https://github.com/gpt4omni/vanta/blob/main/docs/design/apt-repository.md)):

```bash
sudo install -m 0755 -d /etc/apt/keyrings
sudo curl -fsSL https://gpt4omni.github.io/vanta/apt/vanta-archive-keyring.gpg \
    -o /etc/apt/keyrings/vanta-archive-keyring.gpg
echo "deb [signed-by=/etc/apt/keyrings/vanta-archive-keyring.gpg] https://gpt4omni.github.io/vanta/apt stable main" \
    | sudo tee /etc/apt/sources.list.d/vanta.list
sudo apt update
sudo apt install vanta
```

`sudo apt remove vanta` uninstalls; profiles stay in `~/.local/share`. Check
the key against its fingerprint before trusting it, see
[Check what you downloaded](#check-what-you-downloaded).


</Tab>
<Tab title="Fedora / RHEL">


The `.rpm` on the [release page](https://github.com/gpt4omni/vanta/releases/latest)
installs with no repository:

```bash
sudo dnf install ./vanta-*.x86_64.rpm    # or .aarch64.rpm
```

**Only Fedora 44 has a build.** The `.fcNN` in the filename is the Fedora release
it was built against, and only one is built at a time. The repository is split by
`$releasever`, so on any other release the command below returns a 404 rather
than installing a build meant for a different Fedora. That is by design, not a
bug to report ([design note](https://github.com/gpt4omni/vanta/blob/main/docs/design/rpm-repository.md)).

Vanta's own dnf repository, on Fedora 44:

```bash
sudo curl -fsSL https://gpt4omni.github.io/vanta/vanta.repo \
    -o /etc/yum.repos.d/vanta.repo
sudo dnf install vanta
```

Check the key first, see [Check what you downloaded](#check-what-you-downloaded).


</Tab>
<Tab title="Arch">


Install the package from the [release page](https://github.com/gpt4omni/vanta/releases/latest).
It is built by the same `makepkg` run an AUR user would do, and needs no key:

```bash
sudo pacman -U vanta-*-x86_64.pkg.tar.zst
```

Arch is x86-64 only; Arch Linux does not build for aarch64.

**The AUR packages are not published by this project.** `vanta`, `vanta-bin`
and `vanta-git` on the AUR belong to an account that is not the maintainer's
(`taxin-404`). On 2026-09-30 they were copies of
[`packaging/aur/`](https://github.com/gpt4omni/vanta/blob/main/packaging/aur) from 0.17.0, fetching only from this
repository. Nobody here reviews what that account publishes next, so prefer the
release package or Vanta's own repository.

Vanta's own pacman repository. Import its key, then add the stanza to
`pacman.conf`:

```bash
curl -fsSL https://gpt4omni.github.io/vanta/arch/vanta-archive-keyring.asc \
    | sudo pacman-key --add -
sudo pacman-key --lsign-key A75EDC4B9B7383BB2FF0F6BEB5018BF3AB2ACA11
```

```ini
[vanta]
Server = https://gpt4omni.github.io/vanta/arch/$arch
SigLevel = DatabaseRequired PackageNever
```

The database is signed and the packages are not, which is what
`DatabaseRequired PackageNever` says
([design note](https://github.com/gpt4omni/vanta/blob/main/docs/design/pacman-repository.md)). **INFERRED:** `pacman -Sy` and
this `pacman.conf` stanza have not been run; the `SigLevel` line comes from
`pacman.conf(5)`.


</Tab>
<Tab title="Nix">


The flake builds a package for x86-64 and aarch64 Linux. From a checkout:

```bash
nix build '.?submodules=1#vanta'
```

The submodules need `?submodules=1`; flakes do not fetch them otherwise. **It has
never been launched into a game.** What has been run: the package builds, and
`vanta --help` and `vanta --diagnostics` run from the output.
`vanta --diagnostics` reports `Install unknown`, because nothing recognises a
`/nix/store` path yet. The remote form,
`nix run "github:gpt4omni/vanta?submodules=1"`, is **INFERRED** from the local
build and has not been tried. More in
[CONTRIBUTING.md](https://github.com/gpt4omni/vanta/blob/main/CONTRIBUTING.md#or-use-the-flake).


</Tab>
<Tab title="From source">


You do not need this to run Vanta. Build from source if you are changing it,
want your own patches in, or would rather not extend trust to a remote signed by a
key held as a CI secret.

```bash
git clone --recursive https://github.com/gpt4omni/vanta
cd vanta
cargo build --release
```

You need:

- **Clang.** AOSP bionic uses C11 `_Atomic` inside C++ headers and GCC rejects it.
- **GTK4 (4.12 or newer) and libadwaita (1.5 or newer) development packages.**
  Fedora: `dnf install gtk4-devel libadwaita-devel`. Debian/Ubuntu:
  `apt install libgtk-4-dev libadwaita-1-dev`. Arch: `pacman -S gtk4 libadwaita`.
- **Boost's headers**, only for Play in VR, which a plain `cargo build` leaves
  out: build with `--features vanta-runtime/vr` and install `boost-devel`,
  `libboost-dev` or `boost`. It is for dynarmic, the VR mode's translator, which
  `--recursive` fetches. Every package built from this repository's recipes has
  VR turned on; the unsubmitted nixpkgs draft in `packaging/nix/` pins v0.23.2,
  which predates the feature, and does not name it.
- **PipeWire's development headers** (`pipewire-devel`, `libpipewire-0.3-dev`),
  optional. With them the build includes the OpenSL ES audio backend; without
  them there is no sound and everything else works. `libpipewire-0.3.so` is
  loaded at run time, never linked, so a build made with the headers still runs
  on a machine that only has the runtime library, or neither.

To build the Flatpak yourself, which produces what the remote serves, no
submodules needed:

```bash
git clone https://github.com/gpt4omni/vanta
cd vanta
packaging/build-flatpak.sh --install
```

If you change a dependency, run `python3 packaging/cargo-sources.py` in the same
commit as the `Cargo.lock` change, or the Flatpak build fails with
`no matching package`
([issue #3](https://github.com/gpt4omni/vanta/issues/3)).


</Tab>
</Tabs>

## Check what you downloaded

Files on a release page and the package repositories are verified differently.

<Accordion title="Release-page files (cosign)">


Every `.deb`, `.rpm`, `.AppImage` and Arch package on a release page has a
`.cosign.bundle` beside it. The signature is keyless: there is no Vanta signing
key to trust or lose. It proves the file came out of this repository's own
release workflow at that tag. Install
[cosign](https://docs.sigstore.dev/cosign/system_config/installation/), then:

```bash
cosign verify-blob \
  --bundle vanta_0.11.0-1_amd64.deb.cosign.bundle \
  --certificate-identity-regexp '^https://github\.com/gpt4omni/vanta/\.github/workflows/release\.yml@refs/tags/v' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  vanta_0.11.0-1_amd64.deb
```

`Verified OK` is the whole answer. **Do not drop the two `--certificate-*`
flags.** Without them cosign confirms that somebody signed the file, which is not
the question you are asking. Every signature is recorded in Sigstore's public
transparency log.

This covers the release page only. The Flatpak remote and the apt, dnf and
pacman repositories use OpenPGP keys, below.


</Accordion>

<Accordion title="Flatpak remote key">


The published `vanta.flatpakrepo` carries a GPG key, and the repository summary
has a detached signature, so `flatpak install` checks the download was signed by
it. Fingerprint:

```text
225C FE57 D7BC 6D71 8509  D1DA E3C8 4BF5 1338 D38C
```

**A remote added while it was unsigned stays unverified.** Flatpak recorded
`gpg-verify=false` then, and a later signed definition does not change it. If you
added the remote before the key existed, remove and re-add it
(`flatpak remote-delete vanta`, then the `remote-add` above), or import the key:

```bash
curl -fsSL https://gpt4omni.github.io/vanta/vanta.gpg | flatpak remote-modify --gpg-import=- vanta
```

A signature does not make GitHub Pages trustworthy: whoever holds the private
key, a repository secret, can sign anything. That is a weaker arrangement than
Flathub's, and if you would rather not extend that trust, build from source.
Vanta is not on Flathub and, on its current generative-AI policy, cannot be; the
remote is the distribution channel, not a stopgap
([why](https://github.com/gpt4omni/vanta/blob/main/docs/HANDOVER.md#flathub-and-why-it-is-not-the-plan),
[signing procedure](https://github.com/gpt4omni/vanta/blob/main/docs/design/flatpak-remote-signing.md)).


</Accordion>

<Accordion title="apt repository key">


```bash
gpg --show-keys --with-fingerprint /etc/apt/keyrings/vanta-archive-keyring.gpg
```

```text
6A43 861D 0303 1FEA F8E5  BB66 8DD5 F110 EFD5 EF79
```

`dists/stable/InRelease` carries an OpenPGP signature from this key.


</Accordion>

<Accordion title="dnf repository key">


```bash
curl -fsSL https://gpt4omni.github.io/vanta/rpm/RPM-GPG-KEY-vanta | gpg --show-keys
```

```text
13CE 044B 117B AF65 814B  D4E6 97EF BC73 BAC3 53D6
```

The `.repo` file sets `repo_gpgcheck=1` and `gpgcheck=0`: each release
directory's `repodata/repomd.xml` is signed, not the individual `.rpm`
([design note](https://github.com/gpt4omni/vanta/blob/main/docs/design/rpm-repository.md)).


</Accordion>

<Accordion title="pacman repository key">


Fingerprint, as used in the `pacman-key --lsign-key` command above:

```text
A75EDC4B9B7383BB2FF0F6BEB5018BF3AB2ACA11
```

`vanta.db.sig` is verified against this key. The release-page package needs no key
at all, see the cosign section.


</Accordion>
