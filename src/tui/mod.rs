//! App state, input, render, event loop, workers (04, DESIGN). `tui::workers`
//! is the ONE glue point allowed to import source, store, player, resolver, and
//! anilist together (01 §3). Render stays pure: no store writes from draw
//! (01 §5). Loop and shell land with ROD-433 chunk 2.

pub mod clock;
pub mod event;
pub mod workers;
