//! sabigoku library: module map per docs/port/01-modules.md §3.
//! The import-arrow table there is law; each module states its own edges.

pub mod anilist;
pub mod config;
pub mod domain;
pub mod error;
pub mod paths;
pub mod player;
pub mod providers;
pub mod resolve;
pub mod resolver;
pub mod store;
pub mod tui;

pub use error::Error;
