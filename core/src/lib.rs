//! BananaSplit's mixer core, shared by the Linux, Windows and macOS apps.
//!
//! Everything that touches audio samples (`engine`, the 6-band `eq` on
//! every strip and bus, the gate and compressor (`dynamics`) on every strip,
//! the `recorder` that captures them to MP3 or WAV, its own `mp3` encoder and
//! decoder, the `player` that plays recordings back, `ring`, which carries
//! audio between devices' clocks and rates on Windows and macOS, and `vban`,
//! network audio compatible with Voicemeeter), everything that touches
//! persistent state (`config`, `scenes`), the terminal view (`tui`) and the
//! platform-free half of the updater (`release`: release list, checksums,
//! signatures, versions) is plain, dependency-free Rust that can be
//! unit-tested without an audio device. The touch deck page (`deck.html`,
//! [`DECK_PAGE`]) is kept here too: it is the same on every platform.
//!
//! Each platform crate (`linux/`, `windows/`, `macos/`) re-exports these
//! modules from its own library, so its code keeps using
//! `bananasplit::engine::...`, and adds the audio backend, the virtual
//! cables, the GUI and the updater's downloading and installing. The few differences between the systems
//! (where the config folder is, how `~` expands, the wording of the
//! `connect =` hints) are `cfg(target_os)` branches here.

pub mod config;
pub mod dynamics;
pub mod engine;
pub mod eq;
pub mod limiter;
pub mod mp3;
pub mod player;
pub mod recorder;
pub mod release;
pub mod ring;
pub mod scenes;
pub mod tui;
pub mod vban;

/// The touch deck at `/deck`: the everyday controls (faders, mute, solo,
/// routes, record) sized for a small touchscreen, on the same API as the
/// main page. The same page on every platform; the shared GUI layer (`gui/`)
/// serves it with the theme filled in.
pub const DECK_PAGE: &str = include_str!("deck.html");
