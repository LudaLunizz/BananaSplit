<div align="center">

# BananaSplit.

**A low-latency virtual audio mixer for Linux, Windows and macOS, in the spirit of Voicemeeter Banana.**

**[bananasplit.tech](https://bananasplit.tech)**

[![CI](https://github.com/LudaLunizz/BananaSplit/actions/workflows/ci.yml/badge.svg)](https://github.com/LudaLunizz/BananaSplit/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/LudaLunizz/BananaSplit?include_prereleases&sort=semver)](https://github.com/LudaLunizz/BananaSplit/releases)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)

<img src="docs/screenshots/dark.png" alt="BananaSplit mixer window" width="900">

*Ευχαριστώ, φίλε!*

</div>

> [!WARNING]
> BananaSplit is beta software. The main features are in place and work on all three systems, but expect rough edges and frequent releases; see [what beta means here](#what-beta-means).

## Features

- **6 input strips, 5 buses.** V1–V3 are mixes in between, B1 is the mic out (a virtual microphone for Discord, OBS, …) and B2 the system out on your default output. Any bus can play on a device, be a virtual mic, or both, and buses can feed each other (an input into V1, V1 into B2 but not B1: you hear it, the stream doesn’t). V1–V3 were called A1–A3 up to 2.8.24; those names are still read.
- **Per-channel menus** to pick devices, apps and a virtual cable for every channel.
- **Input gain, pan and a 6-band EQ** on every channel, with mute, solo and mono, a **gate and compressor** on every input strip, and a **limiter** on every bus.
- **VBAN network audio**, compatible with Voicemeeter.
- **Multitrack recorder (MP3 or WAV) and player**, plus saved **scenes**. The recorder stops cleanly, and won't start, when less than 1 GB is free on its disk.
- **Borders that tell you at a glance what a channel does.** Green: the selected channel. Blue: the buses playing to the output you listen on. Red: the channels being recorded. Yellow: the buses an app (Discord, OBS) takes sound from (Linux). Several can stack as rings.
- **MIDI controllers.** Learn any fader, knob or button of a hardware MIDI controller to a fader, gain, pan, mute, solo, route, EQ (each band's frequency, gain and Q too), gate and compressor setting, limiter, record arm, player, VBAN stream or scene; motorised faders and LEDs follow along ([MIDI guide](docs/midi.md)). The same commands are open to scripts and decks through the documented [API v1](docs/api.md).
- **Desktop app** with its own window and tray icon. Also runs in a terminal or headless as a service.
- **Updates itself** from the GitHub releases, installing only releases signed with BananaSplit's release key: on request (Menu ▸ Check for updates…), or automatically once you switch that on (off by default).

| Platform | Audio backend | Window |
| --- | --- | --- |
| Linux | JACK client on PipeWire's JACK layer or JACK2, PipeWire virtual cables | GTK 3 + WebKitGTK |
| Windows | WASAPI shared mode, Splitter or VB-Audio cables | WebView2 |
| macOS (new) | Core Audio, BlackHole and other loopback cables | WKWebView |

## Why BananaSplit

As of November 2026, Linux has several good tools in this space. [PulseMeeter](https://github.com/theRealCarneiro/pulsemeeter) is a Voicemeeter-inspired routing mixer on PulseAudio or PipeWire. [PipeWeaver](https://github.com/pipeweaver/pipeweaver) builds virtual sources and matrix mixes on PipeWire, aimed at streaming. [EasyEffects](https://github.com/wwmm/easyeffects) puts chains of effects (many of them plugins) on the input and output of PipeWire apps. On Windows, [Voicemeeter](https://vb-audio.com/Voicemeeter/) is the long-standing virtual mixer this project takes its layout from. Each of them may suit you better; check what they offer today.

What BananaSplit offers is one Voicemeeter Banana-style mixer, with the same strips, buses and window on Linux, Windows and macOS:

- 6 input strips and 5 buses, with a 6-band EQ on every channel, a gate and a compressor on every strip and a limiter on every bus, all built in;
- VBAN network audio that works with Voicemeeter;
- a multitrack recorder (MP3 or WAV) and player, and scenes;
- MIDI controllers (faders, levels, EQ bands, gate and compressor settings, the player and VBAN streams), a local HTTP API (API v1) and a touch deck page;
- an updater that installs only releases signed with BananaSplit's release key.

BananaSplit does not host VST, LV2 or AU plugins. Its EQ, gate, compressor and limiter are its own code.

Some design choices, and the reasons written down for them:

- **A JACK client on Linux, not a native PipeWire client.** The whole mixer runs in one JACK process callback, so every strip and bus is computed for the same frames in the same cycle and the mixer adds no latency of its own ([Sync and latency](linux/README.md#sync-and-latency)). libjack is loaded at run time, so one binary works with JACK2's libjack and with PipeWire's drop-in libjack, and BananaSplit starts and explains what is missing when it isn't there (`linux/src/jack.rs`, `linux/src/dl.rs`). The virtual cables are PipeWire devices, made with `pactl` through PipeWire's PulseAudio layer (`linux/src/cables.rs`).
- **A core without dependencies.** The mixer core (`core/`) uses no third-party crates: the engine, EQ, dynamics, limiter, MP3 encoder and decoder, VBAN, recorder, and the updater's SHA-256 and Ed25519 signature checks are written in the repository. The Linux app adds none either (GTK, WebKitGTK and libjack are loaded at run time). The Windows app adds `tao` and `wry` for its window; the macOS app adds `tao`, `wry` and `tray-icon`.
- **A web page as the window.** The same page (`gui/gui.html`), served by BananaSplit on `127.0.0.1`, is the window on all three systems: in WebKitGTK, WebView2 or WKWebView. So the three apps share one page, one server and one set of commands ([`gui/README.md`](gui/README.md)), and the page also opens in any browser, which is how BananaSplit still shows its window on Linux without WebKitGTK.

## What runs where

| | Linux | Windows | macOS |
| --- | --- | --- | --- |
| Audio | JACK client (JACK2 or PipeWire's JACK layer); the mixer runs at the server's rate | WASAPI shared mode, 48 kHz | Core Audio, 48 kHz (other device rates are converted) |
| Clocks | One graph clock for every port | One device drives the mixer; the others go through drift-correcting rings | Same as Windows |
| Virtual cables | Made by BananaSplit itself as PipeWire devices, removed on quit | [Splitter](https://github.com/LudaLunizz/Splitter) (BananaSplit's own driver, a test build) or VB-Audio cables, installed separately | BlackHole, VB-Cable, Loopback or Soundflower, installed separately |
| Putting an app on a strip | Tick the app in the channel's menu | In Windows' Volume mixer or the app itself; BananaSplit puts apps back on their strip (`app-strips`) | In the app itself |
| Yellow app borders (which buses an app records) | Yes | No | No |
| Updater | Installs the portable package or the .deb (password asked); the Flatpak only checks | Installs the new `bananasplit.exe` (in a folder it can write to) | Checks and opens the release page; you replace the app yourself |
| MIDI backend | ALSA sequencer | WinMM | CoreMIDI |
| `buffer_size` setting | The period asked of PipeWire | Not used | The IO cycle asked of each device |
| Latency documentation | Measured round trips and tuning ([Latency](linux/README.md#latency)) | How the buffers work, no measured figures ([Latency and buffers](windows/README.md#latency-and-buffers)) | The buffer setting, no measured figures ([Devices](macos/README.md#devices)) |
| Local API answers only your own user | Yes (`/proc/net/tcp`) | Yes (Windows' TCP tables) | Yes (the kernel's TCP list; IPv4 only) |
| Quit, record, update, VBAN out need the API token (from 2.9.2) | Yes | Yes | Yes |
| Window and tray | GTK 3 + WebKitGTK, StatusNotifierItem tray | WebView2, notification area | WKWebView, menu bar |
| Packages | .deb, portable .tar.gz, Flatpak | Standalone .exe in a zip | App in a zip, signed ad hoc (not with a Developer ID) |

## What beta means

BananaSplit is past its first rough builds: the mixer, routing, recording and API are in place and used daily. It is still young and changes quickly. In practice:

- **Releases are frequent**, often several in one week. Each one lists its changes in its release notes and in the [Linux manual's version history](linux/README.md#version-history).
- **Your settings keep loading.** A config file, scene or recording made by an older version still loads in a newer one; old names (such as the buses A1 to A3, now V1 to V3) are still read. Going back to an older version can fail, because a newer one may write settings the older one doesn't know; the release notes say when.
- **API v1 only grows.** Anything that would change what an existing [API v1](docs/api.md) command does gets a new version (`/api/v2/`).
- **The window and `/api/cmd` may change.** Controls can move, and the older `/api/cmd` commands the page uses have no such promise; changes to them are named in the release notes.
- **Some parts are tested only with stand-ins**, such as the MIDI support (no real controller yet) and parts of the Linux window and tray. Each manual says which.

## Download

Builds are published on the [Releases](https://github.com/LudaLunizz/BananaSplit/releases) page.

| Platform | File |
| --- | --- |
| Debian, Ubuntu, Mint, Pop!_OS | `bananasplit_<version>_amd64.deb` |
| Any other Linux (x86-64, glibc ≥ 2.34) | `bananasplit-<version>-linux-x86_64.tar.gz` |
| Any Linux with Flatpak (x86-64) | `bananasplit-<version>-x86_64.flatpak` |
| Windows x64 | `bananasplit-<version>-windows-x64.zip` |
| macOS 11 or newer (Apple silicon and Intel) | `bananasplit-<version>-macos.zip` |

Every release ships a `SHA256SUMS.txt`.

Earlier versions, from vmix 1 to BananaSplit 2.7.0, are kept as they were published, one folder per version, in the [`history`](https://github.com/LudaLunizz/BananaSplit/tree/history) branch.

## Install

### Linux

**Debian and derivatives**

```sh
sudo apt install ./bananasplit_<version>_amd64.deb
```

**Other distributions (Arch, Fedora, openSUSE, …)**

```sh
tar -xzf bananasplit-<version>-linux-x86_64.tar.gz
cd bananasplit-<version>-linux-x86_64
./install.sh
```

This installs into `~/.local/bin` and your app menu without touching anything outside your home folder. `./install.sh --uninstall` removes it again.

**Flatpak (any distribution)**

```sh
flatpak install --user bananasplit-<version>-x86_64.flatpak
```

It brings its own dependencies; see the [Linux manual](linux/README.md#install) for what works differently in it.

Runtime dependencies:

| Distro | Audio (required) | Window and tray (recommended) |
| --- | --- | --- |
| Arch | `pipewire-jack libpulse` | `webkit2gtk-4.1` |
| Fedora | `pipewire-jack-audio-connection-kit pulseaudio-utils` | `webkit2gtk4.1` |
| openSUSE | `pipewire-libjack-0_3 pulseaudio-utils` | `libwebkit2gtk-4_1-0` |
| Debian/Ubuntu | `pipewire-jack pulseaudio-utils` | `libwebkit2gtk-4.1-0` |

Closing the window keeps the mixer running in the tray. Quit it from the tray icon or the window menu.

The full Linux manual covering configuration, latency tuning, VBAN and the recorder lives in [`linux/README.md`](linux/README.md).

### Control it from other programs

Every BananaSplit serves a small local HTTP API on `127.0.0.1:8790`, the one its own window uses: read the state and the meters, set faders, start a recording. It is described in [`docs/api.md`](docs/api.md). Mixer controls need nothing more; quitting, recording, updates, opening folders and sending audio over the network also need the token BananaSplit writes at each start to a file only you can read (`bananasplit/BananaSplit.token` next to its port note, see [The token](docs/api.md#the-token)). Scripts read that file; Bitfocus Companion can do every mixer control without it.

### Windows

1. Install the [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) if it is not already present.
2. Extract the zip and run `bananasplit.exe`. It is a single standalone program: no installer, no console window.
3. For per-channel virtual cables, install [Splitter](https://github.com/LudaLunizz/Splitter/releases), BananaSplit's own (a cable for every channel; a test build for now, see the [Windows manual](windows/README.md#virtual-cables)), or [VB-Audio CABLE](https://vb-audio.com/Cable/) (CABLE, CABLE-A … CABLE-D).

BananaSplit lives in the tray: closing the window keeps the mixer running there. **Install to Start Menu** and **Start with Windows** are in the app menu.

See [`windows/README.md`](windows/README.md) for paths and command-line options.

### macOS

The macOS app is new, so older releases don't have it.

1. Unzip `bananasplit-<version>-macos.zip` and drag **BananaSplit** into Applications.
2. Open it. The app isn't signed with an Apple Developer ID, so the first time macOS refuses to open it: allow it with **Open Anyway** in **System Settings › Privacy & Security**, as the [macOS manual](macos/README.md#installing) explains.
3. Allow the microphone when macOS asks.
4. For per-channel virtual cables, install [BlackHole](https://github.com/ExistentialAudio/BlackHole) (`brew install blackhole-2ch`).

BananaSplit lives in the menu bar: closing the window keeps the mixer running there. See [`macos/README.md`](macos/README.md) for devices, cables, paths and command-line options.

To build the app yourself on a Mac (Rust and Xcode's command line tools):

```sh
cd macos
sh packaging/bundle.sh       # dist/BananaSplit.app, for Apple silicon and Intel
```

## Repository layout

```
.
├── core/             shared mixer core (Rust crate, no dependencies): engine, EQ,
│                     dynamics, recorder, player, MP3, VBAN, scenes, config, terminal view
├── gui/              shared GUI layer (source files, not a crate): the HTTP server, the
│                     state and command API and the window process, compiled into each app
├── linux/            Linux app (Rust crate, JACK / PipeWire)
│   ├── src/
│   ├── tests/        unit, end-to-end, GUI and window tests
│   ├── packaging/    .deb and portable package builder
│   ├── contrib/      desktop entry, PipeWire snippets, systemd unit
│   └── assets/
├── windows/          Windows app (Rust crate, WASAPI / WebView2)
├── macos/            macOS app (Rust crate, Core Audio / WKWebView)
├── docs/
│   ├── api.md        the local HTTP API (checked against the real server by the e2e tests)
│   ├── midi.md       MIDI controllers: bindings, modes, learning, midi.json
│   └── screenshots/
└── .github/          CI, release automation, templates
```

The Linux, Windows and macOS apps are separate crates. Each builds on its own, and all three use the mixer in `core/` through a path dependency (`bananasplit-core = { path = "../core" }`) and re-export it, so their code keeps writing `bananasplit::engine::...`. The GUI layer (`gui/`) is one source file set that each app compiles in with `#[path]`; what really differs between the systems is a small `gui_platform` module in each app (see [`gui/README.md`](gui/README.md)). The updater's platform-free half is in `core/`; its download and install half is still per app. `core/` is not a Cargo workspace on purpose: all three apps are packages named `bananasplit`, and each keeps its own `target/`, which the packaging scripts expect.

## Building from source

Requires Rust 1.88 or newer.

**Linux**

```sh
cd linux
cargo build --release
target/release/bananasplit --gui
```

**Windows**

```powershell
cd windows
cargo build --release
target\release\bananasplit.exe
```

**macOS**

```sh
cd macos
cargo build --release
target/release/bananasplit
```

## Testing

```sh
cd core
cargo test                          # the mixer's unit tests (also run on Windows and macOS in CI)
```

```sh
cd linux
cargo test                          # app unit tests and no-allocation tests
sh tests/e2e/run_e2e.sh             # real binary against a mock libjack (gcc, curl, python3)
sh tests/window/run_window_test.sh  # app window and tray on Xvfb (GTK 3, dbus-python, PyGObject)
node tests/gui/ui_check.js          # every GUI control in Chromium (Node.js, Playwright)
sh packaging/build-packages.sh      # builds and verifies the .deb and portable package
sh packaging/build-flatpak.sh       # builds and verifies the Flatpak (flatpak-builder)
```

```sh
cd macos
cargo test                          # unit and no-allocation tests
sh packaging/bundle.sh && sh tests/launch/smoke.sh dist/BananaSplit.app
                                    # builds the app, starts it, asks it, starts it again, quits it
```

```powershell
cd windows
cargo test                          # unit tests
pwsh tests/launch/smoke.ps1 -Exe target\release\bananasplit.exe
                                    # starts the app on the desktop: window, second start, close, quit
```

All of these run in CI on every push and pull request.

## Releasing

1. Bump `version` in `linux/Cargo.toml`, `windows/Cargo.toml` and `macos/Cargo.toml` (`core/` is never published and keeps its own version).
2. Tag the commit with the same version and push the tag:

   ```sh
   git tag v<version>
   git push origin v<version>
   ```

3. When the [release workflow](.github/workflows/release.yml) has finished, run this on the computer that has the release key:

   ```sh
   linux/packaging/sign-release.sh --release v<version>
   ```

4. Write the release's notes on GitHub, then untick pre-release.

The workflow builds the Linux packages, the Windows executable and the macOS app and writes `SHA256SUMS.txt`, but doesn't publish them. `sign-release.sh --release` does, with your own GitHub login (`gh`), so the release is made by your account:

- It takes the files that the workflow left for the tag, but only from a successful run of `release.yml` in this repository.
- It checks the files against `SHA256SUMS.txt` and that the key is one of the two in `core/src/release.rs` (`SIGNING_KEY`, or the backup `BACKUP_SIGNING_KEY`).
- It writes two lines above the checksums, `# BananaSplit <version>` and `# signed <day>`: from 2.8.25 the updater installs a release only when its signed list names that release's version. `sha256sum --check` skips these lines.
- It signs `SHA256SUMS.txt` and publishes everything, `SHA256SUMS.txt.sig` included, as a pre-release with generated notes.

For a release that already exists (the workflow published them itself up to 2.8.2), it only adds those lines to that release's `SHA256SUMS.txt`, signs it and uploads both files.

The in-app updater offers a release once it is published and **not** marked pre-release. It finds the files by their names (`bananasplit-<version>-linux-x86_64.tar.gz`, `bananasplit_<version>_amd64.deb`, `bananasplit-<version>-windows-x64.exe`, `bananasplit-<version>-macos.zip`) and installs a release only when its `SHA256SUMS.txt` is signed with the release key, so keep those names and `SHA256SUMS.txt`.

**The macOS app** is signed ad hoc only (Apple silicon runs nothing unsigned), not with an Apple Developer ID, and not notarized: macOS asks people once to allow it (**Open Anyway**).

**The release key** is kept off GitHub on purpose (not even as a secret), so the workflow can't sign. A release without `SHA256SUMS.txt.sig` is offered but never installed. Keep a backup of the key: without it, no installed BananaSplit can update itself to a new release. From 2.8.25 there is also a **backup key** (`BACKUP_SIGNING_KEY`), kept offline on a separate drive and never on the computer that signs releases. Its signature is accepted too, so if the release key is ever lost or leaked, the next release can be signed with the backup key and carry a new key pair.

## Contributing

Issues and pull requests are welcome. Please run the tests for the platform you touched before opening a pull request.

Developed with help from Claude Code; every change is reviewed, tested in CI and released by the maintainer.

## License

Copyright © 2026 the BananaSplit authors.

BananaSplit is free software: you can redistribute it and/or modify it under the terms of the [GNU General Public License](LICENSE) as published by the Free Software Foundation, either version 3 of the License, or (at your option) any later version. It is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; see the license for details.

Releases up to and including 2.8.22 were published under the MIT License and stay available under it.
