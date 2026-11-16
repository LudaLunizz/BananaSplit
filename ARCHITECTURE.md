# Architecture

BananaSplit is a 6-strip / 5-bus virtual audio mixer for Linux, Windows and macOS.
This file is the map of the code: where things live and which rules hold across
them. For what the program does, read the [README](README.md); for the HTTP API,
[docs/api.md](docs/api.md); for MIDI, [docs/midi.md](docs/midi.md).

## The shape of it

```
            ┌──────────────── one app per system ───────────────┐
            │  linux/    windows/    macos/                      │
            │  audio backend, virtual cables, window, updater    │
            │                                                    │
            │      ┌────────── gui/ (source files) ───────────┐  │
            │      │ HTTP server, state JSON, API v1, MIDI,   │  │
            │      │ the page, the window process             │  │
            │      └───────────────────┬──────────────────────┘  │
            └──────────────────────────┼─────────────────────────┘
                                       │ uses
                      ┌────────────────▼────────────────┐
                      │ core/  (crate bananasplit-core)  │
                      │ mixer engine, EQ, dynamics,      │
                      │ recorder, player, MP3, VBAN,     │
                      │ config, scenes, updater checks   │
                      │ no dependencies, no OS calls     │
                      └─────────────────────────────────┘
```

- **`core/`** is everything that touches audio samples or persistent state. It is
  plain Rust with no dependencies and no calls into the operating system beyond
  `std`, so all of it is unit-tested without an audio device.
- **`linux/`, `windows/`, `macos/`** are the three apps. Each adds the audio
  backend for its system (JACK/PipeWire, WASAPI, Core Audio), the virtual cables,
  the window and the downloading and installing half of the updater. Each
  re-exports the `core` modules from its own library, so app code says
  `bananasplit::engine::...` on every system.
- **`gui/`** is **source files, not a crate**: each app compiles the same files
  into its own binary with `#[path = "../../gui/mod.rs"]`, so there is one copy
  of the server, the commands and the page. What it needs from the system it asks
  of the app's own `gui_platform.rs`. [gui/README.md](gui/README.md) has the file
  table and the platform contract.

### Threads

A running mixer has one **audio thread** (the backend's real-time callback) and
**control threads** (the HTTP server, MIDI, the terminal view, the recorder and
player workers; the window is a separate process that talks to the mixer). They meet in `core::engine::Shared`: atomics for
every setting and meter. Control threads write settings; the audio thread reads
them once per cycle (`Engine` ramps the changes) and writes the meters.

## Codemap: `core/src/`

| File | What it owns |
|---|---|
| `engine.rs` | `Shared` (the settings and meters of 6 strips and 5 buses), `Engine` (turns the strip inputs into the bus outputs once per cycle), routing, gain and pan maths |
| `eq.rs` | the 6-band EQ on every strip and bus |
| `dynamics.rs` | gate and compressor on every input strip |
| `limiter.rs` | the limiter, last stage of each bus |
| `recorder.rs`, `mp3/` | recording to WAV or MP3, with the project's own MP3 encoder and decoder |
| `player.rs` | playing recordings back into the mixer |
| `ring.rs`, `vban/` | carrying audio between devices' clocks and rates; VBAN network audio compatible with Voicemeeter (`proto`, `tx`, `rx`, `resample`) |
| `config.rs` | the ini file: `Config`, its strict parser, atomic save, `ConfigError` |
| `scenes.rs` | scene files (saved mixer states), `SceneError` |
| `release.rs` | the platform-free half of the updater: release list JSON, SHA-256/512, Ed25519 signature check, version rules, `ReleaseError` |
| `tui.rs` | the terminal view |
| `clock.rs`, `util.rs`, `error.rs` | small shared pieces ("is it time again?" checks that never subtract from the clock, `ramp` and `lock`, `IoError`) |
| `deck.html` | the touch deck page served at `/deck`, the same on every system |

## Codemap: the apps

Each app has `src/main.rs` (start-up, the main loop, the command line), `src/lib.rs`
(re-exports), `gui_platform.rs` (the contract with `gui/`) and `update.rs` (download
and install of a release, using `core::release` for every check).

| | `linux/src/` | `windows/src/win/` | `macos/src/mac/` |
|---|---|---|---|
| audio backend | `backend.rs`, `jack.rs` (JACK, loaded with `dlopen`) | `wasapi.rs`, `mod.rs` | `coreaudio.rs`, `catypes.rs`, `mod.rs`; decisions without Core Audio in `pure.rs` |
| system calls | `sys.rs`, `dl.rs` | `ffi.rs` (hand-written declarations, no crates), `sys.rs` | `ffi.rs`, `sys.rs` |
| virtual cables | `cables.rs` (PipeWire) | `cables.rs` (Splitter and others) | `cables.rs` (BlackHole and others) |
| channel menus and routing | `chanmenu.rs`, `devices.rs` | `chanmenu.rs`, `route.rs` | `chanmenu.rs`, `route.rs` |
| MIDI | `midi.rs` (ALSA sequencer, `dlopen`) | `midi.rs` (WinMM) | `midi.rs` (CoreMIDI), plain parts in `pure/midi.rs` |
| window and tray | `shell.rs` | `window.rs`, `tray.rs` | `app.rs` |
| desktop integration | `desktop.rs`, `lowlat.rs` | `desktop.rs`, `console.rs`, `endsession.rs` | `desktop.rs` |
| apps keep their strip | `apphome.rs` | `apphome.rs`, `apppolicy.rs` | none |

`linux/` also holds the packaging (`packaging/`: .deb, Flatpak, install and
`sign-release.sh`), the PipeWire and systemd files (`contrib/`), and the
integration tests (`tests/`).

## Rules that hold everywhere

- **The audio thread does not touch the heap.** The tests
  `linux/tests/*_no_alloc.rs` run the engine, recorder, player and EQ/VBAN paths
  for thousands of cycles, with settings changing underneath as the UI thread
  does, under a counting allocator and assert zero allocations and zero frees.
- **`core` has no dependencies and no OS calls.** Anything that needs the system
  goes in an app crate, behind `gui_platform` or a backend module. The few
  differences between systems that `core` knows about are `cfg(target_os)`
  branches (where the config folder is, how `~` expands).
- **Every `unsafe` block has a `// SAFETY:` comment** (clippy enforces it), and the
  system APIs are declared by hand and loaded at run time where they can be
  missing (JACK, GTK/WebKit and ALSA on Linux).
- **Every public item has a doc comment** (`missing_docs` is a warning in `core`,
  and CI runs clippy with `-D warnings`).
- **Errors are values you can match.** `ConfigError`, `SceneError`,
  `ReleaseError` and `IoError` print the same text a user sees and can be told
  apart by code.
- **The config parser is strict**: an unknown key or a bad value is an error with a
  line number (`unknown key "x" in [global]`), not a silent default. An older version refuses a file a newer one
  wrote when it uses a key the older one does not know.
- **Updates are only installed when `SHA256SUMS.txt` is signed** with the release
  key (or the backup key) and names the version being offered
  (`release::check_release`).
- **The API is documented in `docs/api.md`**, and an end-to-end test checks the
  document against the real server: a new operation or state field needs a row.

## Tests and CI

`.github/workflows/ci.yml` runs, per app and for `core`: `cargo test --locked`,
`cargo fmt --check` and `cargo clippy --locked --all-targets -- -D warnings`, on the
system the app is for (macOS on both Apple targets). On top of that: an
MSRV check (the `rust-version` of all four manifests), `cargo deny` per crate and
target, the Linux end-to-end tests (`linux/tests/e2e/`, against a mock JACK), the
Linux window and tray test (`linux/tests/window/`), the GUI tests in a browser
(`linux/tests/gui/`), and a Windows launch check. `gui/tests/pages/` renders the one
page the way each system serves it, to show that a change meant for one system
did not move the others. `.github/workflows/release.yml` builds the release
packages; signing them is a separate, manual step that uses a key kept off GitHub
(see the README).
