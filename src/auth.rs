//! AniList auth token per 06 §3. Imports: paths (01: `auth` -> `paths`).
//! Kept in its own file, never inside `config`: the bearer must not ride
//! Settings round-trips (06 §3.1).
//!
//! Invariant: read the token through [`AniListAuth::bearer`], never
//! `access_token` directly. That accessor is the single place the control-byte
//! rule (06 §3.3) is enforced; a raw read bypasses it.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::Error;

/// 06 §3.1: oversized auth file loads as the empty signed-out record.
pub const MAX_AUTH_BYTES: u64 = 16 * 1024;

/// AniList returns bearer tokens; the default keeps a hand-written file usable.
pub const DEFAULT_TOKEN_TYPE: &str = "Bearer";

/// Top-level auth record. Nested provider blocks (`.anilist`, later MAL/Kitsu)
/// so a second provider does not reshape the file (06 §3.2).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Auth {
    pub anilist: AniListAuth,
}

/// 06 §3.2 record shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AniListAuth {
    /// Empty = signed out. Read via [`AniListAuth::bearer`], not directly.
    pub access_token: String,
    pub token_type: String,
    /// Unix seconds; `0` = undated, which stays live until a 401 (06 §3.3):
    /// an undated token must not be refused locally.
    pub expires_at: i64,
    /// AniList user id. `MediaListCollection` (the sync pull) needs `> 0`.
    pub user_id: i64,
    pub user_name: String,
}

impl Default for AniListAuth {
    fn default() -> Self {
        AniListAuth {
            access_token: String::new(),
            token_type: DEFAULT_TOKEN_TYPE.into(),
            expires_at: 0,
            user_id: 0,
            user_name: String::new(),
        }
    }
}

impl AniListAuth {
    /// The usable bearer, or `None` when signed out. A control byte (`< 0x20`)
    /// anywhere in the token forces signed-out: a bare LF injects headers, and
    /// CR/LF trips strict HTTP clients' line asserts into a release abort
    /// (06 §3.3). This is the only sanctioned way to reach the token.
    pub fn bearer(&self) -> Option<&str> {
        if self.access_token.is_empty() {
            return None;
        }
        if self.access_token.bytes().any(|b| b < 0x20) {
            return None;
        }
        Some(&self.access_token)
    }

    /// Expired only when dated and reached: a `0` expiry never expires locally
    /// (06 §3.3). Independent of [`bearer`]; a present-but-expired token is the
    /// "reconnect" account state (DESIGN 5.5).
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at != 0 && now >= self.expires_at
    }
}

impl Auth {
    /// Total load (06 §3.1): missing, unreadable, oversized, or corrupt files
    /// all yield the empty signed-out record. Auth never wedges startup.
    pub fn load(path: &Path) -> Auth {
        let Ok(meta) = std::fs::metadata(path) else {
            return Auth::default();
        };
        // Regular files only: reading a FIFO/device blocks forever on open,
        // which would wedge startup (the read runs on the main thread) and
        // break the "total load" invariant. A non-regular file is signed-out.
        if !meta.is_file() || meta.len() > MAX_AUTH_BYTES {
            return Auth::default();
        }
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Write the bearer atomically at mode `0600` (06 §3.1). The temp is
    /// *created* `0600` rather than chmod'd after: a plain create honours the
    /// ambient umask, opening a window where the secret is group/world-readable
    /// on disk before the chmod lands. rename then swaps it in atomically.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        write_new_0600(&tmp, text.as_bytes())?;
        std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
    }
}

/// Create `path` fresh at `0600` and write `bytes`. A stale temp from a prior
/// crash is removed first: `create_new` on an existing file would fail, and
/// reusing it would inherit its old (possibly readable) mode.
fn write_new_0600(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| Error::io(path, e))?;
    f.write_all(bytes).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("sabigoku-auth-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn signed_in() -> Auth {
        Auth {
            anilist: AniListAuth {
                access_token: "abcdefghijklmnopqrstuvwxyz".into(),
                token_type: "Bearer".into(),
                expires_at: 1_800_000_000,
                user_id: 4242,
                user_name: "rod".into(),
            },
        }
    }

    #[test]
    fn round_trip_preserves_a_signed_in_record() {
        let path = tmp("round_trip.toml");
        let auth = signed_in();
        auth.save(&path).unwrap();
        assert_eq!(Auth::load(&path), auth);
    }

    #[test]
    fn default_is_signed_out_bearer_none() {
        let auth = Auth::default();
        assert_eq!(auth.anilist.token_type, "Bearer");
        assert_eq!(auth.anilist.bearer(), None);
    }

    #[test]
    fn missing_file_is_signed_out() {
        assert_eq!(
            Auth::load(Path::new("/nonexistent/auth.toml")),
            Auth::default()
        );
    }

    #[test]
    fn corrupt_file_is_signed_out() {
        let path = tmp("corrupt.toml");
        std::fs::write(&path, "[anilist\naccess_token = ").unwrap();
        assert_eq!(Auth::load(&path), Auth::default());
    }

    #[test]
    fn oversized_file_is_signed_out() {
        let path = tmp("oversized.toml");
        let mut text = String::from("[anilist]\naccess_token = \"x\"\n");
        text.push_str(&"# pad\n".repeat(5_000));
        assert!(text.len() as u64 > MAX_AUTH_BYTES);
        std::fs::write(&path, text).unwrap();
        assert_eq!(Auth::load(&path), Auth::default());
    }

    #[test]
    fn missing_keys_default_missing_block_is_signed_out() {
        let path = tmp("partial.toml");
        std::fs::write(&path, "[anilist]\nuser_name = \"rod\"\n").unwrap();
        let auth = Auth::load(&path);
        assert_eq!(auth.anilist.user_name, "rod");
        assert_eq!(auth.anilist.token_type, "Bearer");
        assert_eq!(auth.anilist.bearer(), None);

        // A file with no [anilist] table at all still loads clean.
        let empty = tmp("empty.toml");
        std::fs::write(&empty, "# nothing here\n").unwrap();
        assert_eq!(Auth::load(&empty), Auth::default());
    }

    #[test]
    fn save_creates_0600_and_leaves_no_temp() {
        let path = tmp("perms.toml");
        let _ = std::fs::remove_file(&path);
        signed_in().save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "auth file must be owner-only, got {mode:o}");
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn save_over_world_readable_stale_temp_still_lands_0600() {
        let path = tmp("stale.toml");
        let stale = path.with_extension("toml.tmp");
        std::fs::write(&stale, "leftover").unwrap();
        std::fs::set_permissions(&stale, std::fs::Permissions::from_mode(0o644)).unwrap();
        signed_in().save(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "stale temp must not leak its mode, got {mode:o}");
    }

    #[test]
    fn save_replaces_an_existing_file() {
        let path = tmp("replace.toml");
        Auth::default().save(&path).unwrap();
        assert_eq!(Auth::load(&path).anilist.bearer(), None);
        signed_in().save(&path).unwrap();
        assert_eq!(Auth::load(&path).anilist.user_name, "rod");
    }

    #[test]
    fn save_failure_surfaces_io_error_with_path() {
        let path = Path::new("/nonexistent-dir-sabigoku/auth.toml");
        match signed_in().save(path) {
            Err(Error::Io { path: p, .. }) => assert_eq!(p, path.with_extension("toml.tmp")),
            other => panic!("expected Error::Io, got {other:?}"),
        }
    }

    #[test]
    fn fifo_path_is_signed_out_not_a_hang() {
        let path = tmp("fifo.toml");
        let _ = std::fs::remove_file(&path);
        let cpath = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
        // SAFETY: mkfifo on a fresh owner-only path (removed just above).
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        // Load off-thread with a deadline: a regressed guard would block on the
        // FIFO open forever, so a hang must fail the test rather than wedge it.
        let (tx, rx) = std::sync::mpsc::channel();
        let p = path.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Auth::load(&p));
        });
        match rx.recv_timeout(std::time::Duration::from_secs(2)) {
            Ok(auth) => assert_eq!(auth, Auth::default()),
            Err(_) => panic!("Auth::load hung on a FIFO"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn control_bytes_force_signed_out() {
        for bad in ["with\nnewline", "with\rcr", "with\ttab", "with\0nul", "\x01lead"] {
            let a = AniListAuth {
                access_token: bad.into(),
                ..AniListAuth::default()
            };
            assert_eq!(a.bearer(), None, "token {bad:?} must be rejected");
        }
        let clean = AniListAuth {
            access_token: "clean-token-value".into(),
            ..AniListAuth::default()
        };
        assert_eq!(clean.bearer(), Some("clean-token-value"));
    }

    #[test]
    fn is_expired_respects_zero_and_boundary() {
        let undated = AniListAuth {
            expires_at: 0,
            ..AniListAuth::default()
        };
        assert!(!undated.is_expired(i64::MAX), "0 expiry never expires");

        let dated = AniListAuth {
            expires_at: 1000,
            ..AniListAuth::default()
        };
        assert!(!dated.is_expired(999));
        assert!(dated.is_expired(1000), "expiry is reached at ==");
        assert!(dated.is_expired(1001));
    }
}
