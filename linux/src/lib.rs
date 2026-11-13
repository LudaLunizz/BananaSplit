//! BananaSplit — a 6-strip / 5-bus virtual audio mixer for Linux.
//!
//! The mixer itself (engine, EQ, dynamics, recorder, release, player, MP3, VBAN,
//! scenes, config and the terminal view) is the shared, dependency-free
//! `bananasplit-core` crate in `../core`, re-exported here so every path
//! stays `bananasplit::engine::...`. The binary (`main.rs`) adds
//! the JACK backend, PipeWire virtual cables, the GUI (`--gui`) and the updater.

pub use bananasplit_core::{
    DECK_PAGE, config, dynamics, engine, eq, limiter, mp3, player, recorder, release, scenes, tui, vban,
};
