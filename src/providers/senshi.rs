//! senshi.live `StreamProvider` (03 §8.2, ROD-441). Plain REST JSON keyed by
//! MAL id: no persisted-query hashes, no AES blob, no captcha from a raw
//! client. Ported from zigoku tag v0.4.7, NOT the freeze 083abd3: the freeze
//! froze a `/anime/filter` body the server now 400s. Provider protocol bytes
//! (query strings, CDN hosts) are live-site facts that track the site; the
//! freeze governs the seam/tiers/walk, not these (08 §10 amend log).
//!
//! API surface: POST /anime/filter (search), GET /episodes/{mal} (list),
//! GET /episode-embeds/{mal}/{ep} (resolve), /posters/{mal}.webp (cover).

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use super::http::{Accept, HttpClient, Method, Request};
use crate::domain::{Enrichment, Quality, StreamLink, Translation, is_still_airing};
use crate::fetchguard::guard_fetch_url;
use crate::providers::{
    CoverRequest, ProviderError, SEARCH_PAGE_SIZE, SearchHit, SearchOptions, StreamProvider,
};

const API: &str = "https://senshi.live";
// Chrome UA: Cloudflare edge serves a plain client with this, no challenge.
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
// Stream CDN (ninstream) 403s a refererless GET; gate on this origin.
const STREAM_REFERER: &str = "https://senshi.live/";
// Cap on a cover ref before splicing into a fetch URL (mirrors allanime).
const MAX_COVER_REF_LEN: usize = 2048;

// ── catalog DTOs ────────────────────────────────────────────────────────────
// Both /anime/filter (`{data:[…]}`) and trending (bare array); `id` is the MAL
// id and the show handle. Serde drops unknown fields by default.

#[derive(Deserialize)]
struct SAnime {
    id: u64,
    title: Option<String>,
    title_english: Option<String>,
    ani_episodes: Option<String>, // JSON string ("16")
    ani_status: Option<String>,
    ani_year: Option<u32>,
}

#[derive(Deserialize)]
struct FilterResp {
    #[serde(default)]
    data: Vec<SAnime>,
}

/// One senshi filter row → tier-C candidate. Provider search feeds binding
/// only (03 §1), so the rich Browse fields (score/season/genres) have no
/// `SearchHit` home. `total_episodes` stays None while airing so a partial
/// aired count never poses as an authoritative total (ROD-419); the aired
/// count still rides `eps_sub` as the fallback episode signal.
fn map_anime(s: SAnime) -> SearchHit {
    let total = s.ani_episodes.as_deref().and_then(parse_leading_uint);
    let status = map_status(s.ani_status.as_deref());
    SearchHit {
        provider_id: s.id.to_string(),
        title: s.title.unwrap_or_else(|| "(untitled)".to_string()),
        title_english: s.title_english,
        title_native: None, // senshi has no separate native field
        anilist_id: None,
        mal_id: Some(s.id as i64),
        total_episodes: if is_still_airing(status.as_deref()) {
            None
        } else {
            total
        },
        // No sub/dub split in the catalog; surface the count as eps_sub.
        eps_sub: total.unwrap_or(0),
        eps_dub: 0,
        year: s.ani_year,
    }
}

/// Fold senshi prose onto the canonical airing vocab (ROD-296). `is_still_airing`
/// only settles exact FINISHED/CANCELLED; raw "Finished Airing" would never
/// auto-complete.
fn map_status(s: Option<&str>) -> Option<String> {
    let v = s?;
    let lower = v.to_ascii_lowercase();
    if lower.contains("finished") {
        Some("FINISHED".into())
    } else if lower.contains("cancel") {
        Some("CANCELLED".into())
    } else if lower.contains("not yet") {
        Some("NOT_YET_RELEASED".into())
    } else if lower.contains("airing") || lower.contains("current") {
        Some("RELEASING".into())
    } else {
        Some(v.to_string()) // unknown → keep raw; is_still_airing defaults safe
    }
}

/// Leading digit run only ("23 min per ep" → 23). None when no leading digit.
fn parse_leading_uint(s: &str) -> Option<u32> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    s[..end].parse().ok()
}

// ── episodes ────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct SEp {
    ep_id: f64,
}

/// Parse /episodes into numerically-sorted labels. Pure over response bytes.
/// Drops phantom ep 0: some shows list a prologue, but /episode-embeds rejects
/// 0 with 400, so offering it only yields an unresolvable pick (ROD-301).
fn parse_episodes(raw: &[u8]) -> Result<Vec<String>, super::ProviderError> {
    let rows: Vec<SEp> = serde_json::from_slice(raw)
        .map_err(|e| super::ProviderError::Decode(format!("episodes: {e}")))?;
    let mut labels: Vec<String> = rows
        .into_iter()
        .filter(|e| e.ep_id != 0.0)
        .map(|e| ep_label(e.ep_id))
        .collect();
    labels.sort_by(|a, b| crate::domain::episode_label_cmp(a, b));
    Ok(labels)
}

/// Integral drops the decimal ("1"); fractional keeps it ("13.5"). Non-finite
/// or negative → "0" (the phantom filtered out upstream).
fn ep_label(n: f64) -> String {
    if !n.is_finite() || n < 0.0 {
        return "0".to_string();
    }
    if n < 1_000_000.0 && n.floor() == n {
        return (n as i64).to_string();
    }
    n.to_string()
}

// ── resolve DTOs + pick logic ───────────────────────────────────────────────

/// One /episode-embeds row: HLS master `url` + `status` track + optional
/// `serverFM` carrying `sub.info=…` for soft-sub sidecars (ROD-378). Status
/// labels lie on some shows; follow the sidecar, do not trust the label.
#[derive(Deserialize, Clone)]
struct Embed {
    url: Option<String>,
    status: Option<String>,
    #[serde(rename = "serverFM")]
    server_fm: Option<String>,
}

/// One track in the sidecar `sub.info` JSON (ROD-378).
#[derive(Deserialize)]
struct SubTrack {
    src: Option<String>,
    label: Option<String>,
    #[serde(default)]
    default: bool,
}

/// Best embed for the track (03 §8.2). Sub: SoftSub > HardSub > other sub; dub
/// is separate and never matches a sub request. Returns the whole embed so the
/// caller can follow `serverFM`. None when the track is not offered.
fn pick_embed(embeds: &[Embed], tt: Translation) -> Option<Embed> {
    embeds
        .iter()
        .filter(|e| e.url.is_some())
        .max_by_key(|e| match_score(e.status.as_deref(), tt))
        .filter(|e| match_score(e.status.as_deref(), tt) > 0)
        .cloned()
}

/// Rank a status label for a track (0 = wrong track). Sub never matches Dub
/// and vice versa.
fn match_score(status: Option<&str>, tt: Translation) -> u8 {
    let Some(s) = status else { return 0 };
    let s = s.to_ascii_lowercase();
    match tt {
        Translation::Dub => u8::from(s.contains("dub")),
        Translation::Sub => {
            if s.contains("dub") {
                0
            } else if s.contains("soft") {
                3
            } else if s.contains("hard") {
                2
            } else if s.contains("sub") {
                1
            } else {
                0
            }
        }
    }
}

/// Pull the percent-decoded `sub.info` value out of `serverFM`. None when
/// absent (a true HardSub).
fn sub_info_url(server_fm: Option<&str>) -> Option<String> {
    let fm = server_fm?;
    let at = fm.find("sub.info=")? + "sub.info=".len();
    let val = &fm[at..];
    let val = val.split('&').next().unwrap_or(val);
    if val.is_empty() {
        return None;
    }
    Some(percent_decode(val))
}

/// Prefer host `default`, then an english-labeled track, then the first
/// (ROD-377).
fn pick_sub_track(tracks: &[SubTrack]) -> Option<String> {
    let mut english = None;
    let mut first = None;
    for t in tracks {
        let Some(src) = t.src.as_deref() else {
            continue;
        };
        if t.default {
            return Some(src.to_string());
        }
        if first.is_none() {
            first = Some(src.to_string());
        }
        if english.is_none()
            && t.label
                .as_deref()
                .is_some_and(|l| l.to_ascii_lowercase().starts_with("eng"))
        {
            english = Some(src.to_string());
        }
    }
    english.or(first)
}

/// Percent-decode a query value. A malformed `%` is kept literal (controls are
/// caught by `clean_arg` before argv); `+` stays literal (raw URL, not form
/// data).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ── input guards ────────────────────────────────────────────────────────────

/// Show id is digits only (stringified MAL id). Reject before URL path splice
/// so `../…` or `1/x` cannot smuggle a second path segment.
fn guard_show_id(show_id: &str) -> Result<(), super::ProviderError> {
    if !show_id.is_empty() && show_id.bytes().all(|c| c.is_ascii_digit()) {
        Ok(())
    } else {
        Err(super::ProviderError::Decode("invalid show id".into()))
    }
}

/// Episode label is `ep_label` shape: digits, at most one `.`. Reject path
/// tricks before URL splice.
fn guard_ep_label(s: &str) -> Result<(), super::ProviderError> {
    if s.is_empty() {
        return Err(super::ProviderError::Decode("invalid episode".into()));
    }
    let mut dots = 0;
    for c in s.bytes() {
        if c == b'.' {
            dots += 1;
            if dots > 1 {
                return Err(super::ProviderError::Decode("invalid episode".into()));
            }
        } else if !c.is_ascii_digit() {
            return Err(super::ProviderError::Decode("invalid episode".into()));
        }
    }
    Ok(())
}

/// Safe for a fetch URL / mpv argv: printable ASCII only (0x21-0x7e). Catches
/// CR/LF and any control a `<0x20` denylist would miss.
fn clean_arg(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| (0x21..=0x7e).contains(&c))
}

fn is_absolute_url(s: &str) -> bool {
    s.starts_with("http://") || s.starts_with("https://")
}

/// Sidecar CDN empty/403 windows (ROD-309): bounded retries with escalating
/// backoff, first try immediate. A dead sidecar must not stall resolve.
const SUB_RETRY_BACKOFFS_MS: [u64; 3] = [300, 700, 1200];

pub struct Senshi {
    http: HttpClient,
    api: String,
}

impl Senshi {
    pub fn new() -> Result<Senshi, ProviderError> {
        Senshi::with_endpoint(API.to_string())
    }

    fn with_endpoint(api: String) -> Result<Senshi, ProviderError> {
        Ok(Senshi {
            http: HttpClient::new()?,
            api,
        })
    }

    /// One API request. `body` present → POST JSON; absent → GET. Any 2xx is
    /// success (ROD-349).
    fn request(
        &self,
        method: Method,
        url: &str,
        body: Option<&str>,
    ) -> Result<Vec<u8>, ProviderError> {
        self.http.fetch(&Request {
            method,
            url,
            payload: body.map(|b| ("application/json", b.as_bytes())),
            user_agent: UA,
            extra_headers: &[("Accept", "application/json")],
            accept: Accept::Any2xx,
            deadline: None,
        })
    }

    /// GET a CDN URL (master playlist, sidecar) with the stream referer. SSRF
    /// guarded, redirects refused (client-wide), 200-only.
    fn cdn_get(&self, url: &str, deadline: Option<Duration>) -> Result<Vec<u8>, ProviderError> {
        guard_fetch_url(url).map_err(|_| ProviderError::Decode("blocked url".into()))?;
        self.http.fetch(&Request {
            method: Method::Get,
            url,
            payload: None,
            user_agent: UA,
            extra_headers: &[("Referer", STREAM_REFERER)],
            accept: Accept::OkOnly,
            deadline,
        })
    }

    /// Fetch the adaptive master and return the variant matching the quality
    /// cap, or None so resolve falls back to the master ladder.
    fn cap_variant(&self, master_url: &str, quality: Quality) -> Option<String> {
        let body = self.cdn_get(master_url, None).ok()?;
        let variants = super::hls::parse_master_playlist(&String::from_utf8_lossy(&body));
        if variants.is_empty() {
            return None; // media playlist: let mpv take the master
        }
        let mut links = Vec::new();
        for v in variants {
            let Some(joined) = super::hls::join_url(master_url, &v.url) else {
                continue;
            };
            if clean_arg(&joined) {
                links.push(StreamLink {
                    url: joined,
                    resolution: v.resolution,
                    referer: None,
                    user_agent: None,
                    cloaked_segments: false,
                    sub_url: None,
                });
            }
        }
        super::hls::select_variant(&links, quality).map(|l| l.url.clone())
    }

    /// Follow `serverFM` to a soft-sub .vtt, or None to play raw. Host URL:
    /// SSRF-guarded, redirects refused, argv-vetted. Any failure yields None
    /// (the stream still plays).
    fn fetch_subtitle(&self, server_fm: Option<&str>) -> Option<String> {
        let info_url = sub_info_url(server_fm)?;
        if !is_absolute_url(&info_url) || !clean_arg(&info_url) {
            return None;
        }
        // First try immediate, then escalating backoff for the CDN's empty/403
        // windows. An empty-body 200 fails the parse and retries.
        let mut tracks: Option<Vec<SubTrack>> = None;
        for attempt in 0..=SUB_RETRY_BACKOFFS_MS.len() {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(SUB_RETRY_BACKOFFS_MS[attempt - 1]));
            }
            if let Ok(body) = self.cdn_get(&info_url, None)
                && let Ok(parsed) = serde_json::from_slice::<Vec<SubTrack>>(&body)
            {
                tracks = Some(parsed);
                break;
            }
        }
        let src = pick_sub_track(&tracks?)?;
        (is_absolute_url(&src) && clean_arg(&src)).then_some(src)
    }
}

impl StreamProvider for Senshi {
    fn name(&self) -> &'static str {
        "senshi"
    }

    fn display_name(&self) -> &'static str {
        "Senshi"
    }

    /// Tier A: the show handle is the stringified MAL id. No MAL id → None,
    /// resolver falls to title search.
    fn canonical_key(&self, show: &Enrichment) -> Option<String> {
        show.mal_id.map(|m| m.to_string())
    }

    /// Server matches title/english/synonyms and ranks by score; trust its
    /// order (a romaji re-rank would drop an English-query hit). v0.4.7 body:
    /// no `languagePreference` (the server 400s it, ROD-442).
    fn search(&self, query: &str, opts: &SearchOptions) -> Result<Vec<SearchHit>, ProviderError> {
        let body = json!({
            "searchTerm": query,
            "types": [], "genres": [], "status": [], "seasons": [],
            "year": "", "studios": [], "producers": [], "languages": [],
            "page": opts.page,
            "limit": SEARCH_PAGE_SIZE,
            "sortBy": "score_desc",
        })
        .to_string();
        let url = format!("{}/anime/filter", self.api);
        let raw = self.request(Method::Post, &url, Some(&body))?;
        let resp: FilterResp = serde_json::from_slice(&raw)
            .map_err(|e| ProviderError::Decode(format!("search: {e}")))?;
        let mut hits: Vec<SearchHit> = resp.data.into_iter().map(map_anime).collect();
        hits.truncate(opts.limit as usize);
        Ok(hits)
    }

    /// Track-agnostic listing; `tt` does not filter here (availability is only
    /// known at embed time). `count_hint` unused: real endpoint.
    fn episodes(
        &self,
        provider_id: &str,
        _tt: Translation,
        _count_hint: Option<u32>,
    ) -> Result<Vec<String>, ProviderError> {
        guard_show_id(provider_id)?;
        let url = format!("{}/episodes/{provider_id}", self.api);
        let raw = self.request(Method::Get, &url, None)?;
        parse_episodes(&raw)
    }

    fn resolve(
        &self,
        provider_id: &str,
        episode: &str,
        tt: Translation,
        quality: Quality,
    ) -> Result<StreamLink, ProviderError> {
        guard_show_id(provider_id)?;
        guard_ep_label(episode)?;
        let url = format!("{}/episode-embeds/{provider_id}/{episode}", self.api);
        let raw = self.request(Method::Get, &url, None)?;
        let embeds: Vec<Embed> = serde_json::from_slice(&raw)
            .map_err(|e| ProviderError::Decode(format!("embeds: {e}")))?;

        // Show/episode exists but the requested track does not: a clean miss,
        // distinct from a transport error.
        let picked = pick_embed(&embeds, tt)
            .ok_or_else(|| ProviderError::Decode("no stream for track".into()))?;
        let stream = picked
            .url
            .ok_or_else(|| ProviderError::Decode("no stream for track".into()))?;
        // Embed url enters mpv argv: absolute http(s) + clean only.
        if !is_absolute_url(&stream) || !clean_arg(&stream) {
            return Err(ProviderError::Decode("bad stream url".into()));
        }

        // `best` leaves mpv on the master ladder; a cap fetches variants.
        // Best-effort: failure falls back to the adaptive master.
        let chosen = if quality == Quality::Best {
            stream.clone()
        } else {
            self.cap_variant(&stream, quality).unwrap_or(stream)
        };
        // Soft subs only on a sub request; do not gate on the status label
        // (ROD-378). None keeps raw play.
        let sub_url = if tt == Translation::Sub {
            self.fetch_subtitle(picked.server_fm.as_deref())
        } else {
            None
        };
        Ok(StreamLink {
            url: chosen,
            resolution: None,
            referer: Some(STREAM_REFERER.to_string()),
            user_agent: Some(UA.to_string()),
            // ninstream serves .ts cloaked as .jpg; mpv must relax its demuxer.
            cloaked_segments: true,
            sub_url,
        })
    }

    /// Cover ref → fetch request. Absolute CDN urls pass through; a relative
    /// `/posters/…webp` gets the host + site referer + UA. Untrusted ref:
    /// bound + printable, or CR/LF/space would smuggle headers.
    fn cover_request(&self, cover_ref: &str) -> Result<CoverRequest, ProviderError> {
        if cover_ref.is_empty() || cover_ref.len() > MAX_COVER_REF_LEN || !clean_arg(cover_ref) {
            return Err(ProviderError::Decode("invalid cover ref".into()));
        }
        if is_absolute_url(cover_ref) {
            return Ok(CoverRequest {
                url: cover_ref.to_string(),
                referer: None,
                user_agent: None,
            });
        }
        let sep = if cover_ref.starts_with('/') { "" } else { "/" };
        Ok(CoverRequest {
            url: format!("{API}{sep}{cover_ref}"),
            referer: Some(STREAM_REFERER.to_string()),
            user_agent: Some(UA.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_anime_maps_a_filter_row() {
        let row = SAnime {
            id: 59708,
            title: Some("Youkoso Jitsuryoku Shijou Shugi no Kyoushitsu e 4th Season".into()),
            title_english: Some("Classroom of the Elite 4th Season".into()),
            ani_episodes: Some("16".into()),
            ani_status: Some("Finished Airing".into()),
            ani_year: Some(2026),
        };
        let h = map_anime(row);
        assert_eq!(h.provider_id, "59708");
        assert_eq!(h.mal_id, Some(59708));
        assert_eq!(
            h.title_english.as_deref(),
            Some("Classroom of the Elite 4th Season")
        );
        assert_eq!(h.total_episodes, Some(16));
        assert_eq!(h.eps_sub, 16);
        assert_eq!(h.eps_dub, 0);
        assert_eq!(h.year, Some(2026));
        assert_eq!(h.title_native, None);
    }

    #[test]
    fn map_anime_withholds_total_while_airing() {
        // A RELEASING catalog count is aired-so-far, not the finale; it must
        // not pose as an authoritative total, but still rides eps_sub.
        let row = SAnime {
            id: 1,
            title: Some("X".into()),
            title_english: None,
            ani_episodes: Some("4".into()),
            ani_status: Some("Currently Airing".into()),
            ani_year: Some(2026),
        };
        let h = map_anime(row);
        assert_eq!(h.total_episodes, None);
        assert_eq!(h.eps_sub, 4);
    }

    #[test]
    fn map_status_folds_senshi_wording() {
        assert_eq!(
            map_status(Some("Finished Airing")).as_deref(),
            Some("FINISHED")
        );
        assert!(!is_still_airing(
            map_status(Some("Finished Airing")).as_deref()
        ));
        assert_eq!(
            map_status(Some("Currently Airing")).as_deref(),
            Some("RELEASING")
        );
        assert!(is_still_airing(
            map_status(Some("Currently Airing")).as_deref()
        ));
        assert_eq!(
            map_status(Some("Not yet aired")).as_deref(),
            Some("NOT_YET_RELEASED")
        );
        assert_eq!(
            map_status(Some("Weird Label")).as_deref(),
            Some("Weird Label")
        );
        assert_eq!(map_status(None), None);
    }

    #[test]
    fn parse_leading_uint_takes_the_digit_prefix() {
        assert_eq!(parse_leading_uint("23 min per ep"), Some(23));
        assert_eq!(parse_leading_uint("16"), Some(16));
        assert_eq!(parse_leading_uint("n/a"), None);
        assert_eq!(parse_leading_uint(""), None);
    }

    #[test]
    fn ep_label_drops_integral_decimal_keeps_fractional() {
        assert_eq!(ep_label(1.0), "1");
        assert_eq!(ep_label(10.0), "10");
        assert_eq!(ep_label(13.5), "13.5");
        assert_eq!(ep_label(f64::NAN), "0");
        assert_eq!(ep_label(-2.0), "0");
    }

    #[test]
    fn parse_episodes_drops_phantom_zero_and_sorts() {
        let raw =
            br#"[{"ep_id":3},{"ep_id":1},{"ep_id":0},{"ep_id":2},{"ep_id":1.5},{"ep_id":10}]"#;
        let eps = parse_episodes(raw).unwrap();
        assert_eq!(eps, ["1", "1.5", "2", "3", "10"]);
    }

    #[test]
    fn match_score_ranks_sub_soft_over_hard_and_separates_dub() {
        assert_eq!(match_score(Some("SoftSub"), Translation::Sub), 3);
        assert_eq!(match_score(Some("HardSub"), Translation::Sub), 2);
        assert_eq!(match_score(Some("Sub"), Translation::Sub), 1);
        assert_eq!(match_score(Some("Dub"), Translation::Sub), 0);
        assert_eq!(match_score(Some("Dub"), Translation::Dub), 1);
        assert_eq!(match_score(Some("SoftSub"), Translation::Dub), 0);
        assert_eq!(match_score(None, Translation::Sub), 0);
    }

    #[test]
    fn pick_embed_takes_the_best_track_or_none() {
        let embeds = [
            Embed {
                url: Some("hard".into()),
                status: Some("HardSub".into()),
                server_fm: None,
            },
            Embed {
                url: Some("soft".into()),
                status: Some("SoftSub".into()),
                server_fm: None,
            },
            Embed {
                url: None,
                status: Some("SoftSub".into()),
                server_fm: None,
            }, // no url, skip
        ];
        assert_eq!(
            pick_embed(&embeds, Translation::Sub)
                .unwrap()
                .url
                .as_deref(),
            Some("soft")
        );
        // Only a dub embed, but a sub was requested → None.
        let dub_only = [Embed {
            url: Some("d".into()),
            status: Some("Dub".into()),
            server_fm: None,
        }];
        assert!(pick_embed(&dub_only, Translation::Sub).is_none());
    }

    #[test]
    fn sub_info_url_extracts_and_percent_decodes() {
        let fm = Some("host=cdn&sub.info=https%3A%2F%2Fcdn%2Finfo.json&x=1");
        assert_eq!(sub_info_url(fm).as_deref(), Some("https://cdn/info.json"));
        assert_eq!(sub_info_url(Some("no sidecar here")), None);
        assert_eq!(sub_info_url(Some("sub.info=")), None);
        assert_eq!(sub_info_url(None), None);
    }

    #[test]
    fn pick_sub_track_prefers_default_then_english_then_first() {
        let tracks = [
            SubTrack {
                src: Some("jp".into()),
                label: Some("Japanese".into()),
                default: false,
            },
            SubTrack {
                src: Some("en".into()),
                label: Some("English".into()),
                default: false,
            },
        ];
        assert_eq!(pick_sub_track(&tracks).as_deref(), Some("en"));

        let with_default = [
            SubTrack {
                src: Some("en".into()),
                label: Some("English".into()),
                default: false,
            },
            SubTrack {
                src: Some("host".into()),
                label: Some("Whatever".into()),
                default: true,
            },
        ];
        assert_eq!(pick_sub_track(&with_default).as_deref(), Some("host"));

        let no_english = [SubTrack {
            src: Some("jp".into()),
            label: Some("Japanese".into()),
            default: false,
        }];
        assert_eq!(pick_sub_track(&no_english).as_deref(), Some("jp"));
        assert_eq!(pick_sub_track(&[]), None);
    }

    #[test]
    fn percent_decode_keeps_malformed_and_plus_literal() {
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("a%2b"), "a+"); // %2b -> '+'
        assert_eq!(percent_decode("a+b"), "a+b"); // literal plus
        assert_eq!(percent_decode("a%zz"), "a%zz"); // malformed kept literal
        assert_eq!(percent_decode("tail%"), "tail%"); // truncated escape
    }

    #[test]
    fn guard_show_id_is_digits_only() {
        assert!(guard_show_id("59708").is_ok());
        assert!(guard_show_id("").is_err());
        assert!(guard_show_id("1/x").is_err());
        assert!(guard_show_id("../7").is_err());
        assert!(guard_show_id("12a").is_err());
    }

    #[test]
    fn guard_ep_label_allows_one_dot() {
        assert!(guard_ep_label("1").is_ok());
        assert!(guard_ep_label("13.5").is_ok());
        assert!(guard_ep_label("").is_err());
        assert!(guard_ep_label("1.2.3").is_err());
        assert!(guard_ep_label("1/2").is_err());
        assert!(guard_ep_label("e1").is_err());
    }

    #[test]
    fn clean_arg_rejects_controls_and_space() {
        assert!(clean_arg("https://cdn/v.m3u8"));
        assert!(!clean_arg("a b"));
        assert!(!clean_arg("a\nb"));
        assert!(!clean_arg("a\r\nb"));
        assert!(!clean_arg(""));
    }

    // ── golden fixtures (live capture 2026-07-18, trimmed) ──────────────────

    const FILTER_FIXTURE: &str = include_str!("../../tests/fixtures/senshi_filter.json");
    const EPISODES_FIXTURE: &str = include_str!("../../tests/fixtures/senshi_episodes.json");

    fn parse_search(raw: &[u8], limit: u32) -> Vec<SearchHit> {
        let resp: FilterResp = serde_json::from_slice(raw).unwrap();
        let mut hits: Vec<SearchHit> = resp.data.into_iter().map(map_anime).collect();
        hits.truncate(limit as usize);
        hits
    }

    #[test]
    fn golden_filter_maps_rows_and_keys_by_mal() {
        let hits = parse_search(FILTER_FIXTURE.as_bytes(), 26);
        assert_eq!(hits.len(), 3);
        let s1 = hits.iter().find(|h| h.provider_id == "52991").unwrap();
        assert_eq!(s1.title, "Sousou no Frieren");
        assert_eq!(s1.mal_id, Some(52991));
        assert_eq!(s1.anilist_id, None); // senshi carries no anilist id
        assert_eq!(s1.total_episodes, Some(28));
        assert_eq!(s1.eps_sub, 28);
        assert_eq!(s1.year, Some(2023));
        assert!(hits.iter().any(|h| h.provider_id == "59978")); // S2
    }

    #[test]
    fn golden_filter_respects_limit() {
        assert_eq!(parse_search(FILTER_FIXTURE.as_bytes(), 1).len(), 1);
    }

    #[test]
    fn golden_episodes_sorts_and_drops_phantom_zero() {
        let eps = parse_episodes(EPISODES_FIXTURE.as_bytes()).unwrap();
        assert_eq!(eps.len(), 28);
        assert_eq!(eps.first().map(String::as_str), Some("1"));
        assert_eq!(eps[9], "10"); // numeric sort, not lexical
        assert_eq!(eps.last().map(String::as_str), Some("28"));
        assert!(!eps.iter().any(|e| e == "0"));
    }

    // ── transport ───────────────────────────────────────────────────────────

    use crate::testutil::{response_with_body, serve_once};

    fn against(response: Vec<u8>) -> Senshi {
        Senshi::with_endpoint(serve_once(response).trim_end_matches('/').to_string()).unwrap()
    }

    #[test]
    fn transport_search_parses_a_2xx_body() {
        let p = against(response_with_body("201 Created", FILTER_FIXTURE.as_bytes()));
        let hits = p
            .search(
                "frieren",
                &SearchOptions {
                    translation: Translation::Sub,
                    limit: 26,
                    page: 1,
                },
            )
            .unwrap();
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn transport_search_forbidden_maps_to_taxonomy() {
        let p = against(response_with_body("403 Forbidden", b""));
        let got = p.search(
            "frieren",
            &SearchOptions {
                translation: Translation::Sub,
                limit: 26,
                page: 1,
            },
        );
        assert!(matches!(got, Err(ProviderError::Forbidden { status: 403 })));
    }

    #[test]
    fn transport_episodes_rejects_a_non_numeric_show_id_before_fetch() {
        // guard_show_id fires before any request; a path-smuggle id never
        // reaches the wire.
        let p = Senshi::new().unwrap();
        assert!(matches!(
            p.episodes("../7", Translation::Sub, None),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn canonical_key_is_the_stringified_mal_id() {
        let p = Senshi::new().unwrap();
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
}
