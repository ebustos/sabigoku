//! Top-level app error. Layers keep their own typed errors (03 §7 taxonomy
//! lands with providers); this enum is the binary-edge catch-all the TUI maps
//! to toast copy, never a stringly `@errorName` port.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HOME is not set and no XDG override provided")]
    NoHome,

    #[error("io at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("config serialize: {0}")]
    ConfigSerialize(#[from] toml::ser::Error),
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io { path: path.into(), source }
    }
}
