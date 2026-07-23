//! User config per 06 §2. Imports: domain, paths (01); domain enums land in
//! ROD-434+, so keys stay strings here and degrade at call site per 06.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// 06 §2.1: oversized file loads as defaults.
pub const MAX_CONFIG_BYTES: u64 = 64 * 1024;

/// All 06 §2.2 keys. Unknown enum strings are kept verbatim and degrade to safe
/// defaults at the call site (a `skip_mode` typo must not disable skipping).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mpv_path: String,
    pub default_quality: String,
    pub translation: String,
    pub resume_offset_sec: u32,
    pub skip_mode: String,
    pub image_protocol: String,
    pub cover_art: bool,
    pub kanji_chips: bool,
    pub palette: String,
    pub landing: String,
    pub title_language: String,
    /// Unclamped as stored; read through `effective_cover_concurrency()`.
    pub discover_cover_concurrency: u32,
    pub preferred_provider: String,
    pub anilist_sync_enabled: bool,
    pub check_for_updates: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            mpv_path: "mpv".into(),
            default_quality: "best".into(),
            translation: "sub".into(),
            resume_offset_sec: 5,
            skip_mode: "both".into(),
            image_protocol: "auto".into(),
            cover_art: true,
            kanji_chips: true,
            palette: "terminal_ghost".into(),
            landing: "last_watched".into(),
            title_language: "romaji".into(),
            discover_cover_concurrency: 4,
            preferred_provider: String::new(),
            anilist_sync_enabled: true,
            check_for_updates: true,
        }
    }
}

impl Config {
    /// Total load (06 §2.1): missing, unreadable, oversized, or corrupt files
    /// all yield defaults. Startup never wedges on config.
    pub fn load(path: &Path) -> Config {
        let Ok(meta) = std::fs::metadata(path) else {
            return Config::default();
        };
        // Regular files only: a FIFO/device would block the read forever and
        // wedge startup (06 §2.1 total load). A non-regular file is defaults.
        if !meta.is_file() || meta.len() > MAX_CONFIG_BYTES {
            return Config::default();
        }
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Save surfaces errors (06 §2.1): Settings and the quit report need
    /// them. Write-to-temp + rename: a crash mid-write must never leave a
    /// torn file that silently loads as defaults (ROD-439 review).
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| Error::io(&tmp, e))?;
        std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
    }

    /// 06 §2.2: clamp to [1, 16] at read.
    pub fn effective_cover_concurrency(&self) -> u32 {
        self.discover_cover_concurrency.clamp(1, 16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("sabigoku-config-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn round_trip_preserves_everything() {
        let path = tmp("round_trip.toml");
        let cfg = Config {
            mpv_path: "/opt/mpv/bin/mpv".into(),
            translation: "dub".into(),
            resume_offset_sec: 12,
            cover_art: false,
            preferred_provider: "allanime".into(),
            ..Config::default()
        };
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path), cfg);
    }

    #[test]
    fn missing_file_is_defaults() {
        assert_eq!(
            Config::load(Path::new("/nonexistent/config.toml")),
            Config::default()
        );
    }

    #[test]
    fn corrupt_file_is_defaults() {
        let path = tmp("corrupt.toml");
        std::fs::write(&path, "mpv_path = [this is not toml").unwrap();
        assert_eq!(Config::load(&path), Config::default());
    }

    #[test]
    fn oversized_file_is_defaults() {
        let path = tmp("oversized.toml");
        let mut text = String::from("mpv_path = \"mpv\"\n");
        text.push_str(&"# pad\n".repeat(20_000));
        assert!(text.len() as u64 > MAX_CONFIG_BYTES);
        std::fs::write(&path, text).unwrap();
        assert_eq!(Config::load(&path), Config::default());
    }

    #[test]
    fn unknown_fields_ignored_missing_fields_defaulted() {
        let path = tmp("partial.toml");
        std::fs::write(&path, "translation = \"dub\"\nfrom_the_future = true\n").unwrap();
        let cfg = Config::load(&path);
        assert_eq!(cfg.translation, "dub");
        assert_eq!(cfg.mpv_path, "mpv");
        assert_eq!(cfg.resume_offset_sec, 5);
    }

    #[test]
    fn save_failure_surfaces_io_error_with_path() {
        // The temp-then-rename write fails at the temp file when the dir is
        // gone; the surfaced path still points into the failing dir.
        let path = Path::new("/nonexistent-dir-sabigoku/config.toml");
        match Config::default().save(path) {
            Err(Error::Io { path: p, .. }) => assert_eq!(p.parent(), path.parent()),
            other => panic!("expected Error::Io, got {other:?}"),
        }
    }

    #[test]
    fn save_is_temp_then_rename_and_leaves_no_temp_behind() {
        let path = tmp("atomic.toml");
        let cfg = Config {
            image_protocol: "kitty".into(),
            ..Config::default()
        };
        cfg.save(&path).unwrap();
        assert!(!path.with_extension("toml.tmp").exists());
        // A non-default value for a key no Settings row surfaces survives
        // the round-trip (the file is fully rewritten, never merged).
        assert_eq!(Config::load(&path).image_protocol, "kitty");
    }

    #[test]
    fn fifo_path_is_defaults_not_a_hang() {
        let path = tmp("fifo.toml");
        let _ = std::fs::remove_file(&path);
        use nix::sys::stat::Mode;
        nix::unistd::mkfifo(&path, Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Config::load(&p));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(cfg) => assert_eq!(cfg, Config::default()),
            Err(_) => panic!("Config::load hung on a FIFO"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn cover_concurrency_clamps_at_read() {
        let mut cfg = Config {
            discover_cover_concurrency: 0,
            ..Config::default()
        };
        assert_eq!(cfg.effective_cover_concurrency(), 1);
        cfg.discover_cover_concurrency = 99;
        assert_eq!(cfg.effective_cover_concurrency(), 16);
        cfg.discover_cover_concurrency = 4;
        assert_eq!(cfg.effective_cover_concurrency(), 4);
    }
}
