//! BananaSplit — a 6-strip / 5-bus virtual audio mixer for Linux.
//!
//! The crate is split so that everything that touches audio samples
//! (`engine`, the 6-band `eq` on every strip and bus, the `recorder` that
//! captures them to WAV, the `player` that plays recordings back, and
//! `vban`, network audio compatible with Voicemeeter) and everything that
//! touches persistent state (`config`, `scenes`) is plain, dependency-free
//! Rust that can be unit-tested without an audio server. The binary (`main.rs`) adds the JACK backend, PipeWire virtual
//! cables, the terminal UI and the GUI (`--gui`).

pub mod config;
pub mod engine;
pub mod eq;
pub mod player;
pub mod recorder;
pub mod scenes;
pub mod vban;
