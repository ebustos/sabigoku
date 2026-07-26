//! sabigoku library: module map per docs/port/01-modules.md §3.
//! The import-arrow table there is law; each module states its own edges.

pub mod anilist;
pub mod aniskip;
pub mod auth;
pub mod cli;
pub mod config;
pub mod domain;
pub mod error;
pub mod fetchguard;
pub mod logging;
pub mod login;
pub mod loopback;
pub mod nonce;
pub mod paths;
pub mod player;
pub mod providers;
pub mod proxy;
pub mod resolve;
pub mod resolver;
pub mod semver;
pub mod store;
pub mod sync;
#[cfg(test)]
pub(crate) mod testutil;
pub mod tui;
pub mod updatecheck;

pub use error::Error;
