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

    #[error("store: {0}")]
    Store(#[from] rusqlite::Error),

    #[error("database schema v{found} is newer than this build supports (v{supported})")]
    SchemaTooNew { found: u32, supported: u32 },

    #[error("migration ladder stopped at v{at}, expected v{expected}")]
    MigrationIncomplete { at: u32, expected: u32 },
}

impl Error {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}
