//! BananaSplit — a 6-strip / 5-bus virtual audio mixer for Windows.
//!
//! The mixer itself (engine, EQ, dynamics, recorder, player, MP3, VBAN,
//! scenes, config and the terminal view) is the shared, dependency-free
//! `bananasplit-core` crate in `../core`, re-exported here so every path
//! stays `bananasplit::engine::...`. The binary (`main.rs`) adds
//! the WASAPI backend, VB-Audio cable handling, the GUI (`--gui`) and the updater.

pub use bananasplit_core::{config, dynamics, engine, eq, mp3, player, recorder, ring, scenes, tui, vban};
