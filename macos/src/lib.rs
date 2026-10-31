//! BananaSplit — a 6-strip / 5-bus virtual audio mixer for macOS.
//!
//! The crate is split so that everything that touches audio samples
//! (`engine`, the 6-band `eq` on every strip and bus, the gate and
//! compressor (`dynamics`) on every strip, the `recorder` that
//! captures them to MP3 or WAV, its own `mp3` encoder and decoder, the
//! `player` that plays recordings back, `ring`, which carries audio between
//! devices' clocks and rates, and `vban`, network audio compatible with
//! Voicemeeter) and everything that touches persistent state (`config`,
//! `scenes`) is plain, dependency-free Rust that can be unit-tested without an
//! audio device. The binary (`main.rs`) adds the Core Audio backend, virtual
//! cable handling, the terminal view and the app (`--gui`).

pub mod config;
pub mod dynamics;
pub mod engine;
pub mod eq;
pub mod mp3;
pub mod player;
pub mod recorder;
pub mod ring;
pub mod scenes;
pub mod vban;
