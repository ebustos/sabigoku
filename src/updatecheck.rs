//! Boot-time update check (06 §6.1). Best-effort: compare the built-in version
//! to GitHub's latest release tag; every failure (offline, rate-limit, bad
//! body, no cache dir) is a silent `None`. Never blocks startup, never
//! surfaces errors. The caller's config (`check_for_updates`) gates whether to
//! run at all. Wall clock and cache dir are parameters; no clock or path
//! resolution in here.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::semver;

/// Ambient re-check window; stays under the GitHub unauth rate limit across
/// launches. 1 hour: zigoku parity (06 §6.1, corrected from a 6h transcription
/// drift with ROD-465).
pub const CHECK_TTL_SECS: i64 = 60 * 60;

/// GitHub rejects requests without a User-Agent (403).
const USER_AGENT: &str = "sabigoku-update-check";

/// Cap on the one GET. Without it a silent host hangs a teardown drain.
const FETCH_TIMEOUT: Duration = Duration::from_secs(3);

/// `/releases/latest` skips prereleases and drafts by definition.
const LATEST_URL: &str = "https://api.github.com/repos/vantroy/sabigoku/releases/latest";

const CACHE_FILE: &str = "update_check";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub checked_at: i64,
    pub latest: String,
}

/// The latest tag only when strictly newer than `current_version`; `None`
/// otherwise and on every failure. `now` is unix seconds.
pub fn check(cache_dir: &Path, current_version: &str, now: i64) -> Option<String> {
    let latest = resolve_latest(cache_dir, now)?;
    semver::is_newer(&latest, current_version).then_some(latest)
}

/// Fresh network tag, bypassing (and refreshing) the cache: `sabigoku update`
/// (06 §6.2) must not act on an hour-old answer. Nothing here is reachable
/// without the network, so coverage lives in tests/update_live.rs.
pub fn latest_fresh(cache_dir: &Path, now: i64) -> Option<String> {
    let tag = fetch_latest()?;
    write_cache(cache_dir, now, &tag);
    Some(tag)
}

fn resolve_latest(cache_dir: &Path, now: i64) -> Option<String> {
    if let Some(entry) = read_cache(cache_dir)
        && is_fresh(entry.checked_at, now)
    {
        return Some(entry.latest);
    }
    let latest = fetch_latest()?;
    write_cache(cache_dir, now, &latest);
    Some(latest)
}

/// Fresh if within TTL and not future-dated: a backward clock or a hand-edited
/// cache must not wedge the check forever. Saturating: `checked_at` is
/// untrusted file input, and a plain subtraction wraps on an extreme negative,
/// which a release build would read as permanently fresh.
pub fn is_fresh(checked_at: i64, now: i64) -> bool {
    checked_at <= now && now.saturating_sub(checked_at) < CHECK_TTL_SECS
}

fn cache_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(CACHE_FILE)
}

/// `None` on any problem: a bad cache is no cache, so the check re-fetches.
/// Bounded like the network body (zigoku caps this same read at 4096); a
/// bloated file truncates into a parse that fails or a tag the cap trims.
fn read_cache(cache_dir: &Path) -> Option<CacheEntry> {
    use std::io::Read;
    let mut text = String::new();
    std::fs::File::open(cache_path(cache_dir))
        .ok()?
        .take(4096)
        .read_to_string(&mut text)
        .ok()?;
    parse_cache(&text)
}

/// Two-line body: `<checked_at>\n<tag>\n`. The tag line goes through the same
/// sanitizer as a network tag: the cache file is untrusted input too, and this
/// path runs on most real boots.
pub fn parse_cache(text: &str) -> Option<CacheEntry> {
    let mut lines = text.lines();
    let checked_at = lines.next()?.trim().parse().ok()?;
    let latest = sanitize_tag(lines.next()?.trim())?;
    Some(CacheEntry { checked_at, latest })
}

/// Printable ASCII, capped at 64; `None` when nothing survives. The one gate
/// both tag sources (release JSON, cache file) pass through, so no consumer
/// (toast copy, Settings row, `update` stdout) can be fed terminal escapes or
/// an unbounded string. zigoku sanitizes at its CLI print instead; hoisting it
/// here covers the TUI path too.
fn sanitize_tag(raw: &str) -> Option<String> {
    let clean: String = raw
        .chars()
        .filter(|c| (' '..='~').contains(c))
        .take(64)
        .collect();
    if clean.is_empty() { None } else { Some(clean) }
}

/// Best-effort; a failure means the next launch re-checks. Write-then-rename:
/// a concurrent boot must never read a truncated half-write, and teardown
/// abandoning this worker mid-write must leave the old cache, not a torn one.
fn write_cache(cache_dir: &Path, now: i64, latest: &str) {
    let _ = std::fs::create_dir_all(cache_dir);
    let tmp = cache_dir.join(format!("{CACHE_FILE}.tmp"));
    let write = std::fs::write(&tmp, format!("{now}\n{latest}\n"))
        .and_then(|()| std::fs::rename(&tmp, cache_path(cache_dir)));
    if let Err(e) = write {
        log::debug!("update check: cache write failed: {e}");
    }
}

/// Cap on the response body. The release JSON is a few KB; a host that can
/// answer as api.github.com (DNS/proxy tampering) must not be able to force
/// an arbitrarily large allocation inside the fetch deadline.
const BODY_CAP: u64 = 64 * 1024;

/// Test seam, compiled in ONLY under the `test-seams` feature (tests/cli.rs,
/// never any distributed binary): lets the suite redirect the fetch and the
/// installer at a local fixture. Without it a `cargo test` would reach GitHub,
/// and `update` would run the real installer over the binary under test.
#[cfg(feature = "test-seams")]
pub(crate) fn url_override(var: &str) -> Option<String> {
    std::env::var(var).ok()
}
#[cfg(not(feature = "test-seams"))]
pub(crate) fn url_override(_var: &str) -> Option<String> {
    None
}

fn fetch_latest() -> Option<String> {
    use std::io::Read;
    let url = url_override("SABIGOKU_UPDATE_URL").unwrap_or_else(|| LATEST_URL.to_string());
    let client = reqwest::blocking::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .user_agent(USER_AGENT)
        .build()
        .ok()?;
    let body = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| log::debug!("update check: fetch failed: {e}"))
        .ok()
        .and_then(|resp| {
            let mut body = String::new();
            resp.take(BODY_CAP).read_to_string(&mut body).ok()?;
            Some(body)
        })?;
    parse_latest_tag(&body)
}

/// Tag from the release JSON, through the `sanitize_tag` gate.
pub fn parse_latest_tag(body: &str) -> Option<String> {
    #[derive(serde::Deserialize)]
    struct LatestRelease {
        tag_name: String,
    }
    let parsed: LatestRelease = serde_json::from_str(body).ok()?;
    sanitize_tag(&parsed.tag_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_fresh_within_ttl_stale_past_and_future() {
        let now: i64 = 1_000_000;
        assert!(is_fresh(now, now));
        assert!(is_fresh(now - (CHECK_TTL_SECS - 1), now));
        assert!(!is_fresh(now - CHECK_TTL_SECS, now));
        assert!(!is_fresh(now + 1, now), "future-dated must re-check");
    }

    /// A release build's wrapped subtraction read i64::MIN as permanently
    /// fresh, silencing the check forever from one edited cache line.
    #[test]
    fn is_fresh_survives_adversarial_extremes() {
        let now: i64 = 1_700_000_000;
        assert!(!is_fresh(i64::MIN, now));
        assert!(!is_fresh(i64::MIN + 1, now));
        assert!(!is_fresh(i64::MAX, now));
        assert!(!is_fresh(-1, now), "negative is just very stale");
    }

    #[test]
    fn parse_cache_valid_two_line_body() {
        let entry = parse_cache("1700000000\nv0.5.0\n").unwrap();
        assert_eq!(entry.checked_at, 1_700_000_000);
        assert_eq!(entry.latest, "v0.5.0");
    }

    #[test]
    fn parse_cache_tolerates_trailing_cr_and_no_final_newline() {
        let entry = parse_cache("1700000000\r\nv0.5.0").unwrap();
        assert_eq!(entry.checked_at, 1_700_000_000);
        assert_eq!(entry.latest, "v0.5.0");
    }

    #[test]
    fn parse_cache_rejects_malformed_bodies() {
        for bad in [
            "",
            "1700000000\n",
            "1700000000\n\n",
            "notanumber\nv0.5.0\n",
            "onlyoneline",
        ] {
            assert_eq!(parse_cache(bad), None, "{bad:?} must not parse");
        }
    }

    /// The cache file is as untrusted as the network body; a poisoned tag
    /// line must come out escape-free and capped, not ride into App state.
    #[test]
    fn parse_cache_sanitizes_and_caps_the_tag_line() {
        let poisoned = "1700000000\nv1.0.0\u{1b}[2J\u{7}\n";
        assert_eq!(parse_cache(poisoned).unwrap().latest, "v1.0.0[2J");
        let flood = format!("1700000000\n{}\n", "x".repeat(10_000_000));
        assert_eq!(parse_cache(&flood).unwrap().latest.len(), 64);
        assert_eq!(parse_cache("1700000000\n\u{1b}\u{7}\n"), None);
    }

    #[test]
    fn write_cache_leaves_no_tmp_behind() {
        let dir = std::env::temp_dir().join("sabigoku-updatecheck-tmpfile");
        let _ = std::fs::remove_dir_all(&dir);
        write_cache(&dir, 1_700_000_000, "v0.9.0");
        assert!(dir.join(CACHE_FILE).is_file());
        assert!(!dir.join(format!("{CACHE_FILE}.tmp")).exists());
    }

    #[test]
    fn parse_latest_tag_pulls_tag_name_ignores_the_rest() {
        let body = r#"{"url":"https://api.github.com/x","tag_name":"v0.5.0","draft":false}"#;
        assert_eq!(parse_latest_tag(body).unwrap(), "v0.5.0");
    }

    #[test]
    fn parse_latest_tag_rejects_no_usable_tag() {
        assert_eq!(parse_latest_tag(r#"{"tag_name":""}"#), None);
        assert_eq!(parse_latest_tag("{}"), None);
        assert_eq!(parse_latest_tag("not json"), None);
    }

    #[test]
    fn parse_latest_tag_strips_terminal_escapes_from_a_forged_tag() {
        assert_eq!(
            parse_latest_tag("{\"tag_name\":\"v1.0\\u001b[2J\"}").unwrap(),
            "v1.0[2J"
        );
        assert_eq!(parse_latest_tag("{\"tag_name\":\"\\u0000\\u0007\"}"), None);
    }

    #[test]
    fn cache_round_trips_through_the_fs() {
        let dir = std::env::temp_dir().join("sabigoku-updatecheck-roundtrip");
        let _ = std::fs::remove_dir_all(&dir);
        write_cache(&dir, 1_700_000_000, "v0.9.0");
        let entry = read_cache(&dir).unwrap();
        assert_eq!(entry.checked_at, 1_700_000_000);
        assert_eq!(entry.latest, "v0.9.0");
    }

    #[test]
    fn check_answers_from_a_fresh_cache_without_network() {
        let dir = std::env::temp_dir().join("sabigoku-updatecheck-fresh");
        let _ = std::fs::remove_dir_all(&dir);
        let now = 1_700_000_000;
        write_cache(&dir, now, "v0.9.0");
        assert_eq!(check(&dir, "0.8.0", now).unwrap(), "v0.9.0");
        assert_eq!(check(&dir, "0.9.0", now), None, "equal is not newer");
        assert_eq!(check(&dir, "1.0.0", now), None, "ahead of latest is silent");
    }
}
