//! BananaSplit's mixer core, shared by the Linux, Windows and macOS apps.
//!
//! Everything that touches audio samples (`engine`, the 6-band `eq` on
//! every strip and bus, the gate and compressor (`dynamics`) on every strip,
//! the `recorder` that captures them to MP3 or WAV, its own `mp3` encoder and
//! decoder, the `player` that plays recordings back, `ring`, which carries
//! audio between devices' clocks and rates on Windows and macOS, and `vban`,
//! network audio compatible with Voicemeeter), everything that touches
//! persistent state (`config`, `scenes`) and the terminal view (`tui`) is
//! plain, dependency-free Rust that can be unit-tested without an audio
//! device.
//!
//! Each platform crate (`linux/`, `windows/`, `macos/`) re-exports these
//! modules from its own library, so its code keeps using
//! `bananasplit::engine::...`, and adds the audio backend, the virtual
//! cables, the GUI and the updater. The few differences between the systems
//! (where the config folder is, how `~` expands, the wording of the
//! `connect =` hints) are `cfg(target_os)` branches here.

pub mod config;
pub mod dynamics;
pub mod engine;
pub mod eq;
pub mod mp3;
pub mod player;
pub mod recorder;
pub mod ring;
pub mod scenes;
pub mod tui;
pub mod vban;
