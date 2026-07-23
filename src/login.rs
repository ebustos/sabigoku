//! OAuth login core (06 §4). Imports: anilist, auth. I/O-free relative to the
//! UI: `complete_login` verifies the token against AniList before writing the
//! file, so a bad token never persists.

use std::path::Path;

use crate::anilist::{AniList, Viewer};
use crate::auth::{AniListAuth, Auth, DEFAULT_TOKEN_TYPE};
use crate::providers::CatalogError;

/// sabigoku's own AniList app (06 §4.1, retiring zigoku's 43536). Registered
/// with redirect `http://localhost:8766`, so the loopback binds 8766, not the
/// 8765 the frozen doc assumed: the redirect must match the registration.
pub const CLIENT_ID: &str = "46528";
pub const LOOPBACK_PORT: u16 = 8766;

const AUTHORIZE_ENDPOINT: &str = "https://anilist.co/api/v2/oauth/authorize";
/// Implicit-grant tokens are ~1y JWTs; anything shorter is not one (06 §4.2).
const TOKEN_FLOOR: usize = 20;

/// The authorize URL without `state`: the paste flow has no CSRF check to
/// anchor one (06 §4.1/§4.3), so it carries none, like zigoku's.
pub fn authorize_url_bare() -> String {
    format!("{AUTHORIZE_ENDPOINT}?client_id={CLIENT_ID}&response_type=token")
}

/// The Implicit-grant authorize URL with the CSRF nonce as `state` (06 §4.1).
pub fn authorize_url(state: &str) -> String {
    format!("{}&state={state}", authorize_url_bare())
}

/// A pasted bare JWT (zigoku extractToken parity) becomes an extractable
/// param; anything else passes through for the normal redirect-URL extract.
pub fn normalize_paste(line: &str) -> String {
    if line.starts_with("eyJ") {
        format!("access_token={line}")
    } else {
        line.to_string()
    }
}

/// Open a URL in the user's browser, best-effort: a failure just means the
/// user opens the printed URL themselves. Detached, never blocks the caller.
pub fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    const OPEN_CMD: &str = "open";
    #[cfg(not(target_os = "macos"))]
    const OPEN_CMD: &str = "xdg-open";
    let _ = std::process::Command::new(OPEN_CMD)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Outcome of a connect attempt, posted to the TUI. Never `Ok` unless the token
/// verified and saved (06 §4.2 states, plus loopback's CSRF/cancel).
#[derive(Debug, Clone, PartialEq)]
pub enum ConnectResult {
    Ok { user_name: String },
    NoToken,
    Rejected,
    NetworkError,
    SaveFailed,
    BadState,
    Canceled,
}

/// The token verify seam (AniList `Viewer`), behind a trait so tests skip the net.
pub trait Verifier {
    fn verify(&self, token: &str) -> Result<Option<Viewer>, CatalogError>;
}

impl Verifier for AniList {
    fn verify(&self, token: &str) -> Result<Option<Viewer>, CatalogError> {
        self.viewer(token)
    }
}

/// Shared login core (06 §4.2): extract the token from a raw redirect/query/
/// paste, floor-check it, verify against AniList, then build and save. The file
/// is never written before the verify succeeds (the save lives only in the
/// verified branch).
pub fn complete_login<V: Verifier>(
    raw: &str,
    verifier: &V,
    auth_path: &Path,
    now: i64,
) -> ConnectResult {
    let Some((token, expires_in)) = extract_token(raw) else {
        return ConnectResult::NoToken;
    };
    if token.len() < TOKEN_FLOOR {
        return ConnectResult::NoToken;
    }
    match verifier.verify(&token) {
        Ok(Some(viewer)) => {
            let auth = build_auth(token, expires_in, &viewer, now);
            match auth.save(auth_path) {
                Ok(()) => ConnectResult::Ok {
                    user_name: viewer.name,
                },
                Err(_) => ConnectResult::SaveFailed,
            }
        }
        Ok(None) => ConnectResult::Rejected,
        Err(_) => ConnectResult::NetworkError,
    }
}

fn build_auth(token: String, expires_in: Option<i64>, viewer: &Viewer, now: i64) -> Auth {
    Auth {
        anilist: AniListAuth {
            access_token: token,
            token_type: DEFAULT_TOKEN_TYPE.into(),
            expires_at: expires_in.map_or(0, |secs| now.saturating_add(secs)),
            user_id: viewer.id,
            user_name: viewer.name.clone(),
        },
    }
}

fn extract_token(raw: &str) -> Option<(String, Option<i64>)> {
    let token = param(raw, "access_token")?;
    let expires_in = param(raw, "expires_in").and_then(|s| s.parse().ok());
    Some((token, expires_in))
}

/// Value of `key=` in a redirect/query/fragment, up to the next delimiter.
/// Implicit-grant values (JWT, integer) carry no chars needing percent-decode.
/// The match is anchored to a param boundary (start or after `&`/`?`/`#`) so a
/// key that is a suffix of another (`xstate=` vs `state=`) can never be read as
/// it: a query parser in a security callback must not be a substring search.
pub(crate) fn param(raw: &str, key: &str) -> Option<String> {
    let needle = format!("{key}=");
    let mut from = 0;
    let idx = loop {
        let hit = from + raw[from..].find(&needle)?;
        if hit == 0 || matches!(raw.as_bytes()[hit - 1], b'&' | b'?' | b'#') {
            break hit;
        }
        from = hit + 1;
    };
    let rest = &raw[idx + needle.len()..];
    let end = rest.find(['&', '#', ' ', '\r', '\n']).unwrap_or(rest.len());
    match &rest[..end] {
        "" => None,
        v => Some(v.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    enum FakeV {
        Ok(Viewer),
        NoViewer,
        Err,
    }
    impl Verifier for FakeV {
        fn verify(&self, _token: &str) -> Result<Option<Viewer>, CatalogError> {
            match self {
                FakeV::Ok(v) => Ok(Some(v.clone())),
                FakeV::NoViewer => Ok(None),
                FakeV::Err => Err(CatalogError::Network),
            }
        }
    }

    fn viewer() -> Viewer {
        Viewer {
            id: 7,
            name: "rod".into(),
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("sabigoku-login-tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    const GOOD_TOKEN: &str = "abcdefghijklmnopqrstuvwxyz012345";

    #[test]
    fn authorize_url_carries_client_response_type_and_state() {
        let u = authorize_url("nonce123");
        assert!(u.contains("client_id=46528"));
        assert!(u.contains("response_type=token"));
        assert!(u.contains("state=nonce123"));
        // The paste flow has no CSRF anchor, so its URL carries no state at all.
        assert!(!authorize_url_bare().contains("state"));
    }

    #[test]
    fn normalize_paste_wraps_a_bare_jwt_and_passes_urls_through() {
        assert_eq!(
            normalize_paste("eyJabc.def.ghi"),
            "access_token=eyJabc.def.ghi"
        );
        let url = "http://localhost:8766/#access_token=tok&state=n";
        assert_eq!(normalize_paste(url), url);
        assert_eq!(normalize_paste(""), "");
    }

    #[test]
    fn complete_login_verifies_then_saves() {
        let path = tmp("ok.toml");
        let _ = std::fs::remove_file(&path);
        let raw =
            format!("http://localhost:8766/#access_token={GOOD_TOKEN}&expires_in=3600&state=n");
        let out = complete_login(&raw, &FakeV::Ok(viewer()), &path, 1000);
        assert_eq!(
            out,
            ConnectResult::Ok {
                user_name: "rod".into()
            }
        );

        let auth = Auth::load(&path);
        assert_eq!(auth.anilist.bearer(), Some(GOOD_TOKEN));
        assert_eq!(auth.anilist.user_id, 7);
        // expires_at = now + expires_in.
        assert_eq!(auth.anilist.expires_at, 1000 + 3600);
    }

    #[test]
    fn absent_expires_in_leaves_undated_token() {
        let path = tmp("undated.toml");
        let raw = format!("?access_token={GOOD_TOKEN}&token_type=Bearer&state=n");
        assert!(matches!(
            complete_login(&raw, &FakeV::Ok(viewer()), &path, 1000),
            ConnectResult::Ok { .. }
        ));
        assert_eq!(Auth::load(&path).anilist.expires_at, 0);
    }

    #[test]
    fn short_or_missing_token_is_no_token_and_writes_nothing() {
        let path = tmp("short.toml");
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            complete_login(
                "#access_token=tooshort&state=n",
                &FakeV::Ok(viewer()),
                &path,
                0
            ),
            ConnectResult::NoToken
        );
        assert_eq!(
            complete_login("#state=n&token_type=Bearer", &FakeV::Ok(viewer()), &path, 0),
            ConnectResult::NoToken
        );
        assert!(!path.exists(), "no verify attempted, nothing to persist");
    }

    #[test]
    fn rejected_and_network_never_persist() {
        let path = tmp("rejected.toml");
        let _ = std::fs::remove_file(&path);
        let raw = format!("#access_token={GOOD_TOKEN}&state=n");
        assert_eq!(
            complete_login(&raw, &FakeV::NoViewer, &path, 0),
            ConnectResult::Rejected
        );
        assert!(!path.exists(), "a rejected token must not be written");

        assert_eq!(
            complete_login(&raw, &FakeV::Err, &path, 0),
            ConnectResult::NetworkError
        );
        assert!(!path.exists(), "an unverified token must not be written");
    }

    #[test]
    fn param_is_anchored_to_a_boundary() {
        // A suffix key must not match the longer one before it.
        assert_eq!(
            param("xstate=evil&state=real", "state").as_deref(),
            Some("real")
        );
        // First real occurrence wins; boundaries are start / & / ? / #.
        assert_eq!(
            param("state=first&state=second", "state").as_deref(),
            Some("first")
        );
        assert_eq!(param("a=1?state=q", "state").as_deref(), Some("q"));
        assert_eq!(
            param("#access_token=tok&x=y", "access_token").as_deref(),
            Some("tok")
        );
        // No real (boundary-anchored) occurrence.
        assert_eq!(param("notstate=nope", "state"), None);
    }

    #[test]
    fn verified_but_unwritable_is_save_failed() {
        let path = Path::new("/nonexistent-dir-sabigoku/auth.toml");
        let raw = format!("#access_token={GOOD_TOKEN}&state=n");
        assert_eq!(
            complete_login(&raw, &FakeV::Ok(viewer()), path, 0),
            ConnectResult::SaveFailed
        );
    }
}
