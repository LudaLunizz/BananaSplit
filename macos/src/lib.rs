//! BananaSplit — a 6-strip / 5-bus virtual audio mixer for macOS.
//!
//! The mixer itself (engine, EQ, dynamics, recorder, release, player, MP3, VBAN,
//! scenes, config and the terminal view) is the shared, dependency-free
//! `bananasplit-core` crate in `../core`, re-exported here so every path
//! stays `bananasplit::engine::...`. The binary (`main.rs`) adds
//! the Core Audio backend, virtual cable handling, the app (`--gui`) and the updater.

pub use bananasplit_core::{
    DECK_PAGE, config, dynamics, engine, eq, mp3, player, recorder, release, ring, scenes, tui, vban,
};

/// The Core Audio backend's decisions as plain functions (tested on every
/// platform).
pub mod pure;
