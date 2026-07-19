//! megaplay.buzz `StreamProvider` (03 §8.1, ROD-445). Tier-A, MAL-keyed:
//! `/stream/mal/{mal}/{ep}/{lang}`. Show handle = stringified MAL id; episode
//! labels are true MAL numbers, so cross-provider watch-state join needs no
//! matching. Ported from zigoku tag v0.4.8, NOT the freeze 083abd3: the
//! decoy-prefix protocol is a live-site fact (v0.4.7 senshi precedent, 08 §10);
//! the freeze governs seam/tiers, not these bytes.
//!
//! No catalog: the MAL route IS the index, so `search` is structurally
//! Unsupported, which must read as "nothing learned", never an absence verdict
//! (03 §8.1): a later mal_id must not be blocked by a stale negative.
//!
//! Two-step resolve, no decryption:
//!   1. GET embed -> scrape `data-id` (the only sub/dub fork). 200 with no
//!      data-id = not stocked.
//!   2. GET /stream/getSources?id=... -> cleartext JSON (master m3u8, softsubs).
//!
//! No listing endpoint: `episodes` probes ep 1, then mints "1".."N" from
//! `count_hint` (03 §8.1). Segments are both fake-extension cloaked (ROD-301)
//! and PNG-header cloaked (ROD-443), so the link sets `cloaked_segments` and
//! `decloak_segments`: playback routes through `proxy::engage`. The whole CDN
//! chain 403s without the megaplay referer + browser UA.

use serde::Deserialize;

use super::http::{Accept, HttpClient, Method, Request};
use crate::domain::{
    Enrichment, MAX_EPISODE_HINT, Quality, StreamLink, Translation, is_absolute_url,
};
use crate::fetchguard::guard_fetch_url;
use crate::providers::{CoverRequest, ProviderError, SearchHit, SearchOptions, StreamProvider};

const HOST: &str = "https://megaplay.buzz";
// Every downstream CDN host gates on this exact origin.
const STREAM_REFERER: &str = "https://megaplay.buzz/";
// Current Chrome UA; a no-UA request 403s on every megaplay host.
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
// Bound on a scraped data-id before URL splice. Real ids are 4-6 digits today.
const MAX_DATA_ID_LEN: usize = 20;
// Cap English subtitle probes per resolve (hostile getSources flood guard).
const MAX_SUBTITLE_PROBES: usize = 6;

// ── getSources JSON ───────────────────────────────────────────────────────────
// Cleartext: sources.file + tracks[]. Absent fields degrade cleanly. serde drops
// unknown fields (intro/outro skip stamps are parked for the ROD-340 player seam).

#[derive(Deserialize)]
struct RawSources {
    file: Option<String>,
}

#[derive(Deserialize)]
struct RawTrack {
    file: Option<String>,
    label: Option<String>,
    kind: Option<String>,
    #[serde(default)]
    default: bool,
}

#[derive(Deserialize)]
struct SourcesResp {
    sources: Option<RawSources>,
    #[serde(default)]
    tracks: Vec<RawTrack>,
}

/// A getSources track that survived argv-vetting (non-null, absolute, clean
/// file). `kind` "thumbnails" is the seekbar sprite, not a subtitle.
#[derive(Clone)]
struct Track {
    file: String,
    label: Option<String>,
    kind: Option<String>,
    default: bool,
}

/// The mapped stream: the mpv-ready link plus the vetted tracks the softsub pick
/// draws from.
struct Sources {
    link: StreamLink,
    tracks: Vec<Track>,
}

/// Pure over response bytes (03 §8.4). Softsub only rides a `sub` resolve
/// (auto-loading dialogue over a dub would be wrong).
fn map_sources(raw: &[u8], tt: Translation) -> Result<Sources, ProviderError> {
    let resp: SourcesResp = serde_json::from_slice(raw)
        .map_err(|e| ProviderError::Decode(format!("getSources: {e}")))?;
    let file = resp
        .sources
        .and_then(|s| s.file)
        .ok_or_else(|| ProviderError::Decode("no stream source".into()))?;
    // Host data -> mpv argv: absolute http(s) + clean argv bytes only.
    if !is_absolute_url(&file) || !clean_arg(&file) {
        return Err(ProviderError::Decode("bad stream url".into()));
    }

    let mut tracks = Vec::new();
    for t in resp.tracks {
        let Some(f) = t.file else { continue };
        // Drop an unsafe track, never fail the stream over it.
        if !is_absolute_url(&f) || !clean_arg(&f) {
            continue;
        }
        tracks.push(Track {
            file: f,
            label: t.label,
            kind: t.kind,
            default: t.default,
        });
    }

    // The picked sub_url goes straight to mpv --sub-file, bypassing the proxy
    // that SSRF-guards the stream. Guard it here or drop it (play raw): argv-vet
    // alone is host-blind, so a track aimed at loopback/metadata would otherwise
    // reach mpv. Deviation past freeze, ratified ROD-445 (backport owed, both
    // providers).
    let sub_url = if tt == Translation::Sub {
        pick_subtitle(&tracks).filter(|u| guard_fetch_url(u).is_ok())
    } else {
        None
    };
    Ok(Sources {
        link: StreamLink {
            url: file,
            resolution: None,
            referer: Some(STREAM_REFERER.to_string()),
            user_agent: Some(UA.to_string()),
            // Segment CDN serves .ts as .jpg (ROD-301) AND prepends a decoy PNG
            // header (ROD-443): mpv needs the relaxed demuxer and the proxy.
            cloaked_segments: true,
            decloak_segments: true,
            sub_url,
        },
        tracks,
    })
}

/// Host `default` wins, else an english-labeled track, else the first
/// subtitle-shaped track (ROD-354). Tracks are already argv-vetted.
fn pick_subtitle(tracks: &[Track]) -> Option<String> {
    let mut english = None;
    let mut first = None;
    for t in tracks {
        if !is_subtitle_track(t) {
            continue;
        }
        if t.default {
            return Some(t.file.clone());
        }
        if english.is_none()
            && t.label
                .as_deref()
                .is_some_and(|l| l.to_ascii_lowercase().starts_with("english"))
        {
            english = Some(t.file.clone());
        }
        if first.is_none() {
            first = Some(t.file.clone());
        }
    }
    english.or(first)
}

/// `captions` or kind-less counts as a subtitle. `thumbnails` and any unknown
/// kind do not (never a wrong `--sub-file`).
fn is_subtitle_track(t: &Track) -> bool {
    match t.kind.as_deref() {
        None => true,
        Some(kind) => kind == "captions",
    }
}

/// English-labeled captions in host order, capped (ROD-377 multi-English
/// disambiguation). Borrows the track files for the cue probe.
fn english_captions(tracks: &[Track]) -> Vec<&str> {
    let mut out = Vec::new();
    for t in tracks {
        if out.len() >= MAX_SUBTITLE_PROBES {
            break;
        }
        if !is_subtitle_track(t) {
            continue;
        }
        let Some(label) = t.label.as_deref() else {
            continue;
        };
        if !label.to_ascii_lowercase().starts_with("english") {
            continue;
        }
        out.push(t.file.as_str());
    }
    out
}

// ── input guards ──────────────────────────────────────────────────────────────

/// Show id is digits only (stringified MAL id). Reject before URL path splice so
/// `../…` or `1/x` cannot smuggle a second path segment.
fn guard_show_id(show_id: &str) -> Result<(), ProviderError> {
    if !show_id.is_empty() && show_id.bytes().all(|c| c.is_ascii_digit()) {
        Ok(())
    } else {
        Err(ProviderError::Decode("invalid show id".into()))
    }
}

/// Safe for a fetch URL / mpv argv: printable ASCII only (0x21-0x7e). Catches
/// CR/LF and any control a `<0x20` denylist would miss.
fn clean_arg(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| (0x21..=0x7e).contains(&c))
}

/// MAL-keyed embed URL. `{sub|dub}` is Translation's wire tag.
fn embed_url(host: &str, mal_id: &str, ep_label: &str, tt: Translation) -> String {
    format!("{host}/stream/mal/{mal_id}/{ep_label}/{}", tt.as_str())
}

/// First numeric `data-id` (quoted or bare), bounded. None when none. Must win
/// over sibling `data-realid` / `data-mediaid` (only `data-id=` matches).
fn parse_data_id(html: &str) -> Option<&str> {
    let needle = "data-id=";
    let bytes = html.as_bytes();
    let mut from = 0;
    while let Some(rel) = html[from..].find(needle) {
        let at = from + rel;
        let mut i = at + needle.len();
        if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i > start && i - start <= MAX_DATA_ID_LEN {
            return Some(&html[start..i]);
        }
        from = at + needle.len();
    }
    None
}

/// Positional "1".."n". A hint-less caller degrades to 1, never 0 (0 collides
/// with the not-stocked verdict, 03 §8.1). Re-clamps: never size an alloc off an
/// unbounded hint.
fn labels(n: u32) -> Vec<String> {
    let count = n.clamp(1, MAX_EPISODE_HINT);
    (1..=count).map(|i| i.to_string()).collect()
}

pub struct MegaPlay {
    http: HttpClient,
    host: String,
}

impl MegaPlay {
    pub fn new() -> Result<MegaPlay, ProviderError> {
        MegaPlay::with_host(HOST.to_string())
    }

    fn with_host(host: String) -> Result<MegaPlay, ProviderError> {
        Ok(MegaPlay {
            http: HttpClient::new()?,
            host,
        })
    }

    /// GET the embed page (existence + sub/dub fork). Referer only; any 2xx.
    fn embed_get(&self, url: &str) -> Result<Vec<u8>, ProviderError> {
        self.http.fetch(&Request {
            method: Method::Get,
            url,
            payload: None,
            user_agent: UA,
            extra_headers: &[("Referer", STREAM_REFERER)],
            accept: Accept::Any2xx,
            deadline: None,
        })
    }

    /// GET getSources as XHR (the host gates the JSON on these headers).
    fn xhr_get(&self, url: &str) -> Result<Vec<u8>, ProviderError> {
        self.http.fetch(&Request {
            method: Method::Get,
            url,
            payload: None,
            user_agent: UA,
            extra_headers: &[
                ("Referer", STREAM_REFERER),
                ("X-Requested-With", "XMLHttpRequest"),
                ("Accept", "application/json"),
            ],
            accept: Accept::Any2xx,
            deadline: None,
        })
    }

    /// Count cue markers in one vtt. SSRF-guarded, redirects refused
    /// (client-wide); the body is counted only, never handed to argv. None on
    /// any failure (a dead sidecar drops the candidate, not the decision).
    fn probe_cues(&self, url: &str) -> Option<usize> {
        guard_fetch_url(url).ok()?;
        let body = self
            .http
            .fetch(&Request {
                method: Method::Get,
                url,
                payload: None,
                user_agent: UA,
                extra_headers: &[("Referer", STREAM_REFERER)],
                accept: Accept::Any2xx,
                deadline: None,
            })
            .ok()?;
        Some(body.windows(5).filter(|w| *w == b" --> ").count())
    }

    /// Upgrade the metadata pick only when a candidate has strictly more cues.
    /// A failed baseline probe keeps metadata (None); a failed candidate probe
    /// drops that candidate, not the decision (ROD-377).
    fn refine_subtitle_by_cues(&self, candidates: &[&str], baseline: &str) -> Option<String> {
        let mut best = baseline;
        let mut best_cues = self.probe_cues(baseline)?;
        for &url in candidates {
            if url == baseline {
                continue;
            }
            let Some(cues) = self.probe_cues(url) else {
                continue;
            };
            if cues > best_cues {
                best = url;
                best_cues = cues;
            }
        }
        if best == baseline {
            None
        } else {
            Some(best.to_string())
        }
    }
}

impl StreamProvider for MegaPlay {
    fn name(&self) -> &'static str {
        "megaplay"
    }

    fn display_name(&self) -> &'static str {
        "MegaPlay"
    }

    /// Tier A: mal_id -> free key. No mal_id -> None; no tier-C recovery.
    fn canonical_key(&self, show: &Enrichment) -> Option<String> {
        show.mal_id.map(|m| m.to_string())
    }

    /// Structurally unsupported: the MAL route is the whole index. Unsupported
    /// must never poison absence (03 §8.1), so it is a distinct error, not an
    /// empty result.
    fn search(&self, _query: &str, _opts: &SearchOptions) -> Result<Vec<SearchHit>, ProviderError> {
        Err(ProviderError::Unsupported)
    }

    /// Listing-less: probe ep 1 (sub, track-agnostic) for existence, then mint
    /// "1".."N" from `count_hint`. `tt` does not filter here; a missing dub
    /// surfaces at resolve, and a dub-mode probe must not read a sub-only show
    /// as not stocked.
    fn episodes(
        &self,
        provider_id: &str,
        _tt: Translation,
        count_hint: Option<u32>,
    ) -> Result<Vec<String>, ProviderError> {
        guard_show_id(provider_id)?;
        let url = embed_url(&self.host, provider_id, "1", Translation::Sub);
        let html = self.embed_get(&url)?;
        let text = String::from_utf8_lossy(&html);
        // 200 with no data-id = authoritative not stocked (03 §8.1).
        if parse_data_id(&text).is_none() {
            return Ok(Vec::new());
        }
        Ok(labels(count_hint.unwrap_or(1)))
    }

    /// Embed (existence + sub/dub fork) then getSources. Two GETs, no
    /// decryption. Quality is ignored: adaptive HLS master, the variant cap
    /// waits on the shared hls follow-up.
    fn resolve(
        &self,
        provider_id: &str,
        episode: &str,
        tt: Translation,
        _quality: Quality,
    ) -> Result<StreamLink, ProviderError> {
        guard_show_id(provider_id)?;
        // Own mint is 1-based integers. A foreign fractional label ("13.5") has
        // no MAL-route address; a u32 parse rejects it before any network. Splice
        // the parsed `n` back, not the raw label: parse accepts "+1"/"007", so
        // the canonical form is what reaches the URL.
        let n: u32 = episode
            .parse()
            .map_err(|_| ProviderError::Decode("invalid episode".into()))?;
        if n == 0 {
            return Err(ProviderError::Decode("invalid episode".into()));
        }

        let embed = embed_url(&self.host, provider_id, &n.to_string(), tt);
        let html = self.embed_get(&embed)?;
        let text = String::from_utf8_lossy(&html);
        // Past-end or missing track (the sub/dub fork lives here), not a
        // transport failure.
        let data_id =
            parse_data_id(&text).ok_or_else(|| ProviderError::Decode("no data-id".into()))?;

        let src_url = format!("{}/stream/getSources?id={data_id}", self.host);
        let raw = self.xhr_get(&src_url)?;
        let mut sources = map_sources(&raw, tt)?;

        // Softsub pick (ROD-377): the host `default` sometimes marks a signs-only
        // track over dialogue; with >=2 English tracks, upgrade to highest-cue by
        // content. Never downgrades below the metadata pick.
        if tt == Translation::Sub
            && let Some(baseline) = sources.link.sub_url.clone()
        {
            let candidates = english_captions(&sources.tracks);
            if candidates.len() >= 2
                && let Some(best) = self.refine_subtitle_by_cues(&candidates, &baseline)
            {
                sources.link.sub_url = Some(best);
            }
        }
        Ok(sources.link)
    }

    /// No covers of its own: an absolute AniList/MAL CDN ref passes through, a
    /// relative or unsafe ref is rejected (never "sanitized" and fetched).
    fn cover_request(&self, cover_ref: &str) -> Result<CoverRequest, ProviderError> {
        if cover_ref.is_empty() || !is_absolute_url(cover_ref) || !clean_arg(cover_ref) {
            return Err(ProviderError::Decode("invalid cover ref".into()));
        }
        Ok(CoverRequest {
            url: cover_ref.to_string(),
            referer: None,
            user_agent: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_data_id_scrapes_the_first_numeric_id_any_quoting() {
        assert_eq!(
            parse_data_id(r#"<div id="megaplay-player" data-id="13458" data-lang="sub">"#),
            Some("13458")
        );
        assert_eq!(parse_data_id("<div data-id='13452'>"), Some("13452"));
        assert_eq!(parse_data_id("<div data-id=7 >"), Some("7"));
        assert_eq!(
            parse_data_id(r#"<a data-id="11"></a><a data-id="22"></a>"#),
            Some("11")
        );
        // data-id wins over sibling data-realid / data-mediaid.
        assert_eq!(
            parse_data_id(
                r#"<div id="megaplay-player" data-id="13461" data-realid="107257" data-mediaid="672">"#
            ),
            Some("13461")
        );
    }

    #[test]
    fn parse_data_id_skips_empty_nonnumeric_and_bounds_the_id() {
        assert_eq!(
            parse_data_id(r#"<a data-id=""></a><b data-id="x9"></b><c data-id="42"></c>"#),
            Some("42")
        );
        assert_eq!(parse_data_id("<html>no ids here</html>"), None);
        assert_eq!(parse_data_id(""), None);
        // Over-long digit runs rejected, not spliced into a URL.
        assert_eq!(parse_data_id(r#"data-id="123456789012345678901""#), None);
    }

    #[test]
    fn guard_show_id_accepts_mal_rejects_traversal() {
        assert!(guard_show_id("52991").is_ok());
        assert!(guard_show_id("").is_err());
        assert!(guard_show_id("../etc").is_err());
        assert!(guard_show_id("52991/x").is_err());
        assert!(guard_show_id("13458abc").is_err());
    }

    #[test]
    fn embed_url_splices_mal_episode_and_track() {
        assert_eq!(
            embed_url(HOST, "52991", "28", Translation::Sub),
            "https://megaplay.buzz/stream/mal/52991/28/sub"
        );
        assert_eq!(
            embed_url(HOST, "21", "1100", Translation::Dub),
            "https://megaplay.buzz/stream/mal/21/1100/dub"
        );
    }

    #[test]
    fn labels_mints_positional_and_degrades_hintless_to_one() {
        let eps = labels(28);
        assert_eq!(eps.len(), 28);
        assert_eq!(eps.first().map(String::as_str), Some("1"));
        assert_eq!(eps.last().map(String::as_str), Some("28"));
        // Zero -> one (empty means not-stocked). Hostile count clamps.
        assert_eq!(labels(0).len(), 1);
        assert_eq!(labels(u32::MAX).len(), MAX_EPISODE_HINT as usize);
    }

    #[test]
    fn clean_arg_rejects_controls_and_space() {
        assert!(clean_arg("https://cdn/v.m3u8"));
        assert!(!clean_arg("a b"));
        assert!(!clean_arg("a\nb"));
        assert!(!clean_arg("a\r\nb"));
        assert!(!clean_arg(""));
    }

    #[test]
    fn map_sources_maps_a_live_shaped_body() {
        let raw = br#"{"sources":{"file":"https://cdn.mewstream.buzz/x/master.m3u8"},
            "tracks":[
              {"file":"https://1oe.lostproject.club/eng.vtt","label":"English","kind":"captions","default":true},
              {"file":"https://1oe.lostproject.club/thumbs.vtt","kind":"thumbnails"},
              {"label":"ghost-no-file","kind":"captions"}],
            "intro":{"start":100,"end":190},"server":2}"#;
        let s = map_sources(raw, Translation::Sub).unwrap();
        assert_eq!(s.link.url, "https://cdn.mewstream.buzz/x/master.m3u8");
        assert_eq!(s.link.referer.as_deref(), Some(STREAM_REFERER));
        assert_eq!(s.link.user_agent.as_deref(), Some(UA));
        assert!(s.link.cloaked_segments);
        assert!(s.link.decloak_segments);
        // Two vetted tracks (the file-less ghost dropped); thumbnails kept as a
        // track but never the sub pick.
        assert_eq!(s.tracks.len(), 2);
        assert_eq!(
            s.link.sub_url.as_deref(),
            Some("https://1oe.lostproject.club/eng.vtt")
        );
        // A dub resolve never loads a softsub.
        let d = map_sources(raw, Translation::Dub).unwrap();
        assert_eq!(d.link.sub_url, None);
    }

    #[test]
    fn map_sources_missing_or_unsafe_stream_url_is_a_clean_error() {
        assert!(matches!(
            map_sources(b"{}", Translation::Sub),
            Err(ProviderError::Decode(_))
        ));
        assert!(matches!(
            map_sources(br#"{"sources":{}}"#, Translation::Sub),
            Err(ProviderError::Decode(_))
        ));
        assert!(matches!(
            map_sources(
                br#"{"sources":{"file":"/x/master.m3u8"}}"#,
                Translation::Sub
            ),
            Err(ProviderError::Decode(_))
        ));
        assert!(matches!(
            map_sources(
                br#"{"sources":{"file":"https://cdn/x master.m3u8"}}"#,
                Translation::Sub
            ),
            Err(ProviderError::Decode(_))
        ));
        // An unsafe track is dropped, not fatal, and cannot become the sub pick.
        let s = map_sources(
            br#"{"sources":{"file":"https://cdn/ok.m3u8"},"tracks":[{"file":"/relative.vtt","kind":"captions"}]}"#,
            Translation::Sub,
        )
        .unwrap();
        assert_eq!(s.tracks.len(), 0);
        assert_eq!(s.link.sub_url, None);
    }

    #[test]
    fn map_sources_drops_a_sub_url_aimed_at_a_private_host() {
        // The picked sub_url bypasses the proxy and reaches mpv --sub-file; a
        // default track pointing at cloud metadata / loopback must not survive
        // the SSRF guard, while the stream itself (proxy-guarded) still plays.
        for host in [
            "http://169.254.169.254/latest/meta-data/",
            "http://127.0.0.1:9/pwn.vtt",
        ] {
            let raw = format!(
                r#"{{"sources":{{"file":"https://cdn/ok.m3u8"}},"tracks":[{{"file":"{host}","label":"English","kind":"captions","default":true}}]}}"#
            );
            let s = map_sources(raw.as_bytes(), Translation::Sub).unwrap();
            assert_eq!(s.link.sub_url, None, "{host} must be guarded out");
            assert_eq!(s.link.url, "https://cdn/ok.m3u8");
        }
    }

    fn track(file: &str, label: Option<&str>, kind: Option<&str>, default: bool) -> Track {
        Track {
            file: file.to_string(),
            label: label.map(str::to_string),
            kind: kind.map(str::to_string),
            default,
        }
    }

    #[test]
    fn pick_subtitle_default_wins_english_next_first_fallback_thumbnails_never() {
        let thumbs = track("https://c/thumbs.vtt", None, Some("thumbnails"), true);
        let spanish = track(
            "https://c/spa.vtt",
            Some("Spanish"),
            Some("captions"),
            false,
        );
        let english = track(
            "https://c/eng.vtt",
            Some("English - CR"),
            Some("captions"),
            false,
        );
        let eng_default = track(
            "https://c/eng2.vtt",
            Some("English"),
            Some("captions"),
            true,
        );
        let bare = track("https://c/bare.vtt", None, None, false);

        // Host default beats an earlier english; english beats first; thumbnails
        // never qualify.
        assert_eq!(
            pick_subtitle(&[thumbs.clone(), english.clone(), eng_default]).as_deref(),
            Some("https://c/eng2.vtt")
        );
        assert_eq!(
            pick_subtitle(&[spanish.clone(), english]).as_deref(),
            Some("https://c/eng.vtt")
        );
        assert_eq!(
            pick_subtitle(&[thumbs, spanish]).as_deref(),
            Some("https://c/spa.vtt")
        );
        assert_eq!(
            pick_subtitle(&[bare]).as_deref(),
            Some("https://c/bare.vtt")
        );
        assert_eq!(
            pick_subtitle(&[track("https://c/t.vtt", None, Some("thumbnails"), true)]),
            None
        );
        assert_eq!(pick_subtitle(&[]), None);
        // Unknown kind fails safe (never a wrong --sub-file).
        let alien = track("https://c/alien.vtt", None, Some("chapters"), true);
        assert_eq!(pick_subtitle(&[alien]), None);
    }

    #[test]
    fn english_captions_only_english_labeled_captions_host_order_capped() {
        let tracks = vec![
            track(
                "https://c/eng-3.vtt",
                Some("English"),
                Some("captions"),
                true,
            ),
            track(
                "https://c/spa.vtt",
                Some("Spanish"),
                Some("captions"),
                false,
            ),
            track(
                "https://c/eng-4.vtt",
                Some("English 2"),
                Some("captions"),
                false,
            ),
            track(
                "https://c/thumbs.vtt",
                Some("English"),
                Some("thumbnails"),
                false,
            ),
            track("https://c/eng-bare.vtt", Some("English"), None, false),
            track("https://c/bare.vtt", None, Some("captions"), false),
        ];
        // Kind-less "English" qualifies; Spanish, thumbnails (even labeled
        // English), and unlabeled do not.
        assert_eq!(
            english_captions(&tracks),
            vec![
                "https://c/eng-3.vtt",
                "https://c/eng-4.vtt",
                "https://c/eng-bare.vtt"
            ]
        );
        assert_eq!(english_captions(&[]).len(), 0);

        let flood: Vec<Track> = (0..12)
            .map(|_| track("https://c/e.vtt", Some("English"), Some("captions"), false))
            .collect();
        assert_eq!(english_captions(&flood).len(), MAX_SUBTITLE_PROBES);
    }

    // ── provider surface (no network: guards fire before the wire) ────────────

    #[test]
    fn canonical_key_is_the_stringified_mal_id() {
        let p = MegaPlay::new().unwrap();
        let with_mal = Enrichment {
            anilist_id: 1,
            mal_id: Some(52991),
            ..Enrichment::default()
        };
        assert_eq!(p.canonical_key(&with_mal).as_deref(), Some("52991"));
        let no_mal = Enrichment {
            anilist_id: 1,
            ..Enrichment::default()
        };
        assert_eq!(p.canonical_key(&no_mal), None);
    }

    #[test]
    fn search_is_structurally_unsupported() {
        let p = MegaPlay::new().unwrap();
        let got = p.search(
            "frieren",
            &SearchOptions {
                translation: Translation::Sub,
                limit: 26,
                page: 1,
            },
        );
        assert!(matches!(got, Err(ProviderError::Unsupported)));
    }

    #[test]
    fn resolve_rejects_foreign_or_corrupt_episode_labels_before_any_network() {
        let p = MegaPlay::new().unwrap();
        for bad in ["13.5", "0", ""] {
            assert!(
                matches!(
                    p.resolve("52991", bad, Translation::Sub, Quality::Best),
                    Err(ProviderError::Decode(_))
                ),
                "episode {bad:?} must reject before the wire"
            );
        }
        assert!(matches!(
            p.resolve("../x", "1", Translation::Sub, Quality::Best),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn cover_request_absolute_passes_relative_and_unsafe_reject() {
        let p = MegaPlay::new().unwrap();
        let abs = p
            .cover_request("https://s4.anilist.co/file/cover.jpg")
            .unwrap();
        assert_eq!(abs.url, "https://s4.anilist.co/file/cover.jpg");
        assert_eq!(abs.referer, None);
        assert!(p.cover_request("/posters/x.webp").is_err());
        assert!(p.cover_request("").is_err());
        assert!(p.cover_request("https://cdn/x y.jpg").is_err());
    }

    // ── transport (single-hop paths against the one-shot mock) ────────────────

    use crate::testutil::{response_with_body, serve_once};

    fn against(response: Vec<u8>) -> MegaPlay {
        MegaPlay::with_host(serve_once(response).trim_end_matches('/').to_string()).unwrap()
    }

    #[test]
    fn episodes_empty_when_embed_has_no_data_id() {
        // 200 with no data-id = authoritative not stocked (one embed GET).
        let p = against(response_with_body("200 OK", b"<html>no player here</html>"));
        let eps = p.episodes("52991", Translation::Sub, Some(12)).unwrap();
        assert!(eps.is_empty());
    }

    #[test]
    fn episodes_mints_from_hint_when_embed_is_stocked() {
        let p = against(response_with_body(
            "200 OK",
            br#"<div id="megaplay-player" data-id="13458">"#,
        ));
        let eps = p.episodes("52991", Translation::Sub, Some(12)).unwrap();
        assert_eq!(eps.len(), 12);
        assert_eq!(eps.first().map(String::as_str), Some("1"));
        assert_eq!(eps.last().map(String::as_str), Some("12"));
    }

    #[test]
    fn episodes_rejects_a_non_numeric_show_id_before_fetch() {
        let p = MegaPlay::new().unwrap();
        assert!(matches!(
            p.episodes("../7", Translation::Sub, None),
            Err(ProviderError::Decode(_))
        ));
    }
}
