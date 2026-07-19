//! Platform dirs per 06 §1. Imports nothing from the crate (01: `paths` is a leaf).
//! Linux XDG only; Windows is unsupported at freeze (no silent scatter).

use std::path::{Path, PathBuf};

use crate::error::Error;

pub const APP_SEGMENT: &str = "sabigoku";
pub const DB_FILE: &str = "sabigoku.db";
pub const CONFIG_FILE: &str = "config.toml";
pub const AUTH_FILE: &str = "auth.toml";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `config.toml`, `auth.toml`
    pub config: PathBuf,
    /// `sabigoku.db`, debug log
    pub data: PathBuf,
    /// covers, AniSkip scripts, update-check cache
    pub cache: PathBuf,
    /// mpv IPC sockets. Must resolve even without HOME.
    pub runtime: PathBuf,
}

impl Paths {
    /// Resolve from the process environment.
    pub fn resolve() -> Result<Paths, Error> {
        Self::resolve_from(|k| std::env::var(k).ok())
    }

    /// XDG var wins when set and non-empty, else the HOME-based default.
    /// Runtime never needs HOME: `$XDG_RUNTIME_DIR/sabigoku` or `/tmp/sabigoku`.
    pub fn resolve_from(var: impl Fn(&str) -> Option<String>) -> Result<Paths, Error> {
        let var = |k: &str| var(k).filter(|v| !v.is_empty());
        let home = var("HOME");
        let base = |xdg: &str, home_suffix: &str| -> Result<PathBuf, Error> {
            match var(xdg) {
                Some(dir) => Ok(PathBuf::from(dir).join(APP_SEGMENT)),
                None => match &home {
                    Some(h) => Ok(PathBuf::from(h).join(home_suffix).join(APP_SEGMENT)),
                    None => Err(Error::NoHome),
                },
            }
        };
        Ok(Paths {
            config: base("XDG_CONFIG_HOME", ".config")?,
            data: base("XDG_DATA_HOME", ".local/share")?,
            cache: base("XDG_CACHE_HOME", ".cache")?,
            runtime: match var("XDG_RUNTIME_DIR") {
                Some(dir) => PathBuf::from(dir).join(APP_SEGMENT),
                None => PathBuf::from("/tmp").join(APP_SEGMENT),
            },
        })
    }

    pub fn config_file(&self) -> PathBuf {
        self.config.join(CONFIG_FILE)
    }

    pub fn auth_file(&self) -> PathBuf {
        self.config.join(AUTH_FILE)
    }

    pub fn db_file(&self) -> PathBuf {
        self.data.join(DB_FILE)
    }

    /// Best-effort mkdir -p on all four dirs; real failure surfaces on open (06 §1).
    pub fn ensure_dirs(&self) {
        for dir in [&self.config, &self.data, &self.cache, &self.runtime] {
            let _ = std::fs::create_dir_all(dir);
        }
        // The mpv IPC socket lives under runtime; on the /tmp fallback (no
        // XDG_RUNTIME_DIR) that dir is world-visible, so lock it to the owner
        // rather than trust the ambient umask. Owner-created dirs only.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.runtime, std::fs::Permissions::from_mode(0o700));
    }
}

/// Collapse a leading `$HOME` to `~` for display only. Boundary-safe:
/// `/home/rod` must not swallow `/home/rodney` (06 §1).
pub fn collapse_home(path: &Path, home: &Path) -> String {
    let display = path.display().to_string();
    let home = home.display().to_string();
    if home.is_empty() || home == "/" {
        return display;
    }
    let home = home.strip_suffix('/').unwrap_or(&home);
    if display == home {
        return "~".to_string();
    }
    match display.strip_prefix(home) {
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => display,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn resolve(pairs: &[(&str, &str)]) -> Result<Paths, Error> {
        let map = env(pairs);
        Paths::resolve_from(|k| map.get(k).cloned())
    }

    #[test]
    fn home_defaults() {
        let p = resolve(&[("HOME", "/home/rod")]).unwrap();
        assert_eq!(p.config, PathBuf::from("/home/rod/.config/sabigoku"));
        assert_eq!(p.data, PathBuf::from("/home/rod/.local/share/sabigoku"));
        assert_eq!(p.cache, PathBuf::from("/home/rod/.cache/sabigoku"));
        assert_eq!(p.runtime, PathBuf::from("/tmp/sabigoku"));
    }

    #[test]
    fn xdg_overrides_win() {
        let p = resolve(&[
            ("HOME", "/home/rod"),
            ("XDG_CONFIG_HOME", "/cfg"),
            ("XDG_DATA_HOME", "/dat"),
            ("XDG_CACHE_HOME", "/cch"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
        ])
        .unwrap();
        assert_eq!(p.config, PathBuf::from("/cfg/sabigoku"));
        assert_eq!(p.data, PathBuf::from("/dat/sabigoku"));
        assert_eq!(p.cache, PathBuf::from("/cch/sabigoku"));
        assert_eq!(p.runtime, PathBuf::from("/run/user/1000/sabigoku"));
    }

    #[test]
    fn empty_xdg_var_is_unset() {
        let p = resolve(&[("HOME", "/home/rod"), ("XDG_CONFIG_HOME", "")]).unwrap();
        assert_eq!(p.config, PathBuf::from("/home/rod/.config/sabigoku"));
    }

    #[test]
    fn no_home_errors_but_runtime_would_resolve() {
        assert!(matches!(resolve(&[]), Err(Error::NoHome)));
        let p = resolve(&[
            ("XDG_CONFIG_HOME", "/cfg"),
            ("XDG_DATA_HOME", "/dat"),
            ("XDG_CACHE_HOME", "/cch"),
        ])
        .unwrap();
        assert_eq!(p.runtime, PathBuf::from("/tmp/sabigoku"));
    }

    #[test]
    fn file_locations() {
        let p = resolve(&[("HOME", "/h")]).unwrap();
        assert_eq!(
            p.config_file(),
            PathBuf::from("/h/.config/sabigoku/config.toml")
        );
        assert_eq!(
            p.auth_file(),
            PathBuf::from("/h/.config/sabigoku/auth.toml")
        );
        assert_eq!(
            p.db_file(),
            PathBuf::from("/h/.local/share/sabigoku/sabigoku.db")
        );
    }

    #[test]
    fn ensure_dirs_creates_all_four() {
        let root = std::env::temp_dir()
            .join("sabigoku-paths-tests")
            .join("ensure");
        let _ = std::fs::remove_dir_all(&root);
        let p = Paths {
            config: root.join("cfg"),
            data: root.join("data"),
            cache: root.join("cache"),
            runtime: root.join("run"),
        };
        p.ensure_dirs();
        for dir in [&p.config, &p.data, &p.cache, &p.runtime] {
            assert!(dir.is_dir(), "{} not created", dir.display());
        }
        p.ensure_dirs();
    }

    #[test]
    fn collapse_home_is_boundary_safe() {
        let home = Path::new("/home/rod");
        assert_eq!(collapse_home(Path::new("/home/rod/x"), home), "~/x");
        assert_eq!(collapse_home(Path::new("/home/rod"), home), "~");
        assert_eq!(
            collapse_home(Path::new("/home/rodney/x"), home),
            "/home/rodney/x"
        );
        assert_eq!(collapse_home(Path::new("/etc"), home), "/etc");
        assert_eq!(collapse_home(Path::new("/x"), Path::new("/")), "/x");
    }
}
