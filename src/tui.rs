//! App state, input, render, event loop, workers (04, DESIGN). `tui::workers`
//! is the ONE glue point allowed to import source, store, player, resolver, and
//! anilist together (01 §3). Render stays pure: no store writes from draw
//! (01 §5). Filled in ROD-439.
