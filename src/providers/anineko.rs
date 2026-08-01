//! anineko.to `StreamProvider` (ROD-520). Slug-keyed with no canonical
//! endpoint: `canonical_key` never answers, so every bind runs through title
//! search and the pure scorers. The payload carries no id and no year, so
//! episode count is the only corroborating signal the scorer ever gets.
//!
//! Chain: /ajax/search (JSON) -> /watch/{slug} (episode hrefs) ->
//! /watch/{slug}/ep-{n} (server buttons, sub/dub fork, inline softsub in the
//! embed URL) -> embed page (`const src` m3u8, cleartext) -> master.
//!
//! Segments are PNG-header cloaked on a shared ad CDN, so the link sets
//! `cloaked_segments` + `decloak_segments` and playback routes through
//! `proxy::engage`. The IEND box sits at byte 62 but the TS sync only starts at
//! 252: `proxy::decloak` scans for the sync triple rather than trusting the PNG
//! length, so the gap costs nothing. Do not "optimize" that into a fixed-offset
//! skip.
//!
//! Nothing in the chain gates on Referer or User-Agent. Both are sent anyway so
//! a host that turns gating on does not take the provider down with it; the two
//! headers are deliberate, not vestigial.

use serde::Deserialize;

use super::http::{Accept, HttpClient, Method, Request};
use crate::domain::{
    Enrichment, MAX_EPISODE_HINT, Quality, StreamLink, Translation, is_absolute_url,
};
use crate::fetchguard::guard_fetch_url;
use crate::providers::{
    CoverRequest, ProviderError, SearchHit, SearchOptions, StreamProvider, clean_arg,
};

const HOST: &str = "https://anineko.to";
const REFERER: &str = "https://anineko.to/";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
/// Bound on a slug before it reaches a URL path splice.
const MAX_SLUG_LEN: usize = 160;
/// Embed hosts tried per resolve. Five to six are listed per track; the tail is
/// the same stream behind flakier hosts.
const MAX_EMBED_TRIES: usize = 3;
/// Sidecar extensions mpv accepts as `--sub-file`.
const SUB_EXTENSIONS: [&str; 3] = [".vtt", ".srt", ".ass"];

// -- search DTOs -------------------------------------------------------------

#[derive(Deserialize)]
struct SearchResult {
    title: Option<String>,
    url: Option<String>,
    meta: Option<String>,
}

#[derive(Deserialize)]
struct SearchResp {
    #[serde(default)]
    results: Vec<SearchResult>,
}

// -- pure parsers ------------------------------------------------------------

/// ASCII alphanumeric, hyphen, underscore, bounded. Rejects the path
/// separators, dots, and query bytes that would let a stored id escape its
/// `/watch/{slug}` segment.
fn guard_slug(slug: &str) -> Result<(), ProviderError> {
    let ok = !slug.is_empty()
        && slug.len() <= MAX_SLUG_LEN
        && slug
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_');
    if ok {
        Ok(())
    } else {
        Err(ProviderError::Decode("invalid show id".into()))
    }
}

/// Slug out of a `/watch/{slug}` result url. Anything with extra path segments
/// is refused rather than truncated: a shape change should surface as a miss,
/// not as a confidently wrong id.
fn slug_from_url(url: &str) -> Option<&str> {
    let slug = url.trim_start_matches('/').strip_prefix("watch/")?;
    let slug = slug.trim_end_matches('/');
    if slug.is_empty() || slug.contains('/') {
        return None;
    }
    guard_slug(slug).ok()?;
    Some(slug)
}

/// Episode count out of `"TV • 28 Episodes"`. The suffix is required: the same
/// slot carries a bare year (`"TV • 1999"` for One Piece), and reading that as
/// a count makes the scorer reject the right show. Unparseable yields None,
/// never 0.
fn meta_episodes(meta: &str) -> Option<u32> {
    let (_, rest) = meta.split_once('\u{2022}')?;
    let rest = rest.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    // Case-insensitive: the count is the scorer's only corroborating signal
    // here, and a byte-exact match would drop it site-wide on "28 episodes".
    let suffix = rest[digits.len()..].trim_start().to_ascii_lowercase();
    if digits.is_empty() || !suffix.starts_with("episode") {
        return None;
    }
    digits.parse().ok().filter(|&n| n > 0)
}

/// Map the search payload. A result missing a title or a usable slug is
/// dropped, never bound with a placeholder.
fn parse_search(raw: &[u8]) -> Result<Vec<SearchHit>, ProviderError> {
    let resp: SearchResp =
        serde_json::from_slice(raw).map_err(|e| ProviderError::Decode(format!("search: {e}")))?;
    Ok(resp
        .results
        .into_iter()
        .filter_map(|r| {
            let title = r.title.filter(|t| !t.is_empty())?;
            let slug = slug_from_url(r.url.as_deref()?)?;
            Some(SearchHit {
                provider_id: slug.to_string(),
                title,
                total_episodes: r.meta.as_deref().and_then(meta_episodes),
                ..SearchHit::default()
            })
        })
        .collect())
}

/// Sorted, deduplicated, clamped episode numbers.
///
/// No quote and no leading slash in the needle: anchoring on `"/watch/` makes a
/// single-quoted or relative href scan as zero episodes. `/ep-` is what keeps a
/// sibling slug out.
fn parse_episode_numbers(html: &str, slug: &str) -> Vec<u32> {
    let needle = format!("watch/{slug}/ep-");
    let mut nums = Vec::new();
    let mut from = 0;
    while let Some(rel) = html[from..].find(&needle) {
        let at = from + rel + needle.len();
        let digits: String = html[at..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        from = at;
        if let Ok(n) = digits.parse::<u32>()
            && n > 0
        {
            nums.push(n);
        }
    }
    nums.sort_unstable();
    nums.dedup();
    nums.truncate(MAX_EPISODE_HINT as usize);
    nums
}

/// One playable embed off the episode page.
#[derive(Debug, PartialEq)]
struct Server {
    url: String,
    track: Translation,
}

/// Server buttons in page order. The track label sits in a `<span>` AFTER the
/// `data-video` attribute inside the same button, so the classification scans
/// forward to `</button>`, not backward.
fn parse_servers(html: &str) -> Vec<Server> {
    let needle = "data-video=\"";
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(rel) = html[from..].find(needle) {
        let start = from + rel + needle.len();
        let Some(end_rel) = html[start..].find('"') else {
            break;
        };
        let end = start + end_rel;
        let url = &html[start..end];
        // End-of-document fallback would classify the last server against the
        // whole page, so a stray "dub" in a footer flips its track.
        let Some(tail_rel) = html[end..].find("</button>") else {
            break;
        };
        let tail_end = end + tail_rel;
        out.push(Server {
            url: url.to_string(),
            track: track_of(&html[end..tail_end]),
        });
        from = tail_end.max(end + 1);
    }
    out
}

/// "dub" anywhere in the button label means dub; everything else is sub. The
/// live sub label is "Sort Sub", which must not be read as a dub.
fn track_of(button_tail: &str) -> Translation {
    if button_tail.to_ascii_lowercase().contains("dub") {
        Translation::Dub
    } else {
        Translation::Sub
    }
}

/// The `const src = "..."` master URL on an embed page. Cleartext, no key
/// exchange. Keeps scanning past a near-miss: the needle is a prefix of
/// `const srcSet` / `const source`, and one of those earlier in the page must
/// not hide a real `const src` further down.
fn parse_embed_src(html: &str) -> Option<&str> {
    let needle = "const src";
    let mut from = 0;
    while let Some(rel) = html[from..].find(needle) {
        let at = from + rel;
        from = at + needle.len();
        if let Some(found) = embed_src_at(&html[from..]) {
            return Some(found);
        }
    }
    None
}

/// One `const src` site: the assignment must follow immediately, else this was
/// a longer identifier and the caller keeps scanning.
fn embed_src_at(after_needle: &str) -> Option<&str> {
    let rest = after_needle.trim_start();
    let rest = rest.strip_prefix('=')?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let inner = &rest[1..];
    let end = inner.find(quote)?;
    Some(&inner[..end]).filter(|s| !s.is_empty())
}

/// Inline softsub carried in the embed URL's query. The parameter name differs
/// per embed host (`sub`, `caption_1`, `c1_file`), so the pick is value-shaped:
/// the first absolute http(s) value whose path ends in a subtitle extension.
/// That skips the sibling label params (`sub_1=English`) without hardcoding a
/// host list that a new embed host would fall outside of.
fn subtitle_from_embed_url(embed_url: &str) -> Option<String> {
    let parsed = url::Url::parse(embed_url).ok()?;
    parsed
        .query_pairs()
        .map(|(_, v)| v.into_owned())
        .find(|v| is_absolute_url(v) && has_subtitle_extension(v))
}

/// What may become a play URL, or one we fetch under a quality cap. The master
/// is scraped off a third-party embed page: the site's choice, not ours (03 §6.7).
fn master_ok(url: &str) -> bool {
    is_absolute_url(url) && clean_arg(url) && guard_fetch_url(url).is_ok()
}

/// Vetted master off an embed page. `None` means hop: a refused master must not
/// reach `StreamLink.url`, where the refusal resurfaces at play time.
fn embed_master(embed: &str) -> Option<&str> {
    parse_embed_src(embed).filter(|m| master_ok(m))
}

/// Extension test against the path only: a `?`/`#` tail must not decide which
/// param is the sidecar. This is param SELECTION, not a security gate; the
/// gates are `clean_arg` + `guard_fetch_url` in `guarded_subtitle`.
fn has_subtitle_extension(url: &str) -> bool {
    let path = url
        .split_once(['?', '#'])
        .map_or(url, |(head, _)| head)
        .to_ascii_lowercase();
    SUB_EXTENSIONS.iter().any(|ext| path.ends_with(ext))
}

// -- provider ----------------------------------------------------------------

pub struct Anineko {
    http: HttpClient,
    host: String,
}

impl Anineko {
    pub fn new() -> Result<Anineko, ProviderError> {
        Anineko::with_host(HOST.to_string())
    }

    fn with_host(host: String) -> Result<Anineko, ProviderError> {
        Ok(Anineko {
            http: HttpClient::new()?,
            host,
        })
    }

    fn page_get(&self, url: &str, referer: &str) -> Result<Vec<u8>, ProviderError> {
        self.http.fetch(&Request {
            method: Method::Get,
            url,
            payload: None,
            user_agent: UA,
            extra_headers: &[
                ("Referer", referer),
                (
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                ),
            ],
            accept: Accept::Any2xx,
            deadline: None,
        })
    }

    fn ajax_get(&self, url: &str) -> Result<Vec<u8>, ProviderError> {
        self.http.fetch(&Request {
            method: Method::Get,
            url,
            payload: None,
            user_agent: UA,
            extra_headers: &[
                ("Referer", REFERER),
                ("X-Requested-With", "XMLHttpRequest"),
                ("Accept", "application/json"),
            ],
            accept: Accept::Any2xx,
            deadline: None,
        })
    }

    /// Variant matching the quality cap, or None to fall back to the master
    /// ladder. The guard is the one that stops the request leaving the process;
    /// the caller vets the same URL before it can become a play URL.
    fn cap_variant(&self, master_url: &str, referer: &str, quality: Quality) -> Option<String> {
        guard_fetch_url(master_url).ok()?;
        let body = self.page_get(master_url, referer).ok()?;
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
                    decloak_segments: false,
                    sub_url: None,
                });
            }
        }
        let pick = super::hls::select_variant(&links, quality)?;
        Some(pick.url.clone())
    }

    /// Softsub off the embed URL, dropped unless it survives the SSRF guard and
    /// argv vetting: `sub_url` goes straight to mpv `--sub-file`, bypassing the
    /// proxy that guards the stream itself.
    fn guarded_subtitle(&self, embed_url: &str) -> Option<String> {
        let url = subtitle_from_embed_url(embed_url)?;
        (clean_arg(&url) && guard_fetch_url(&url).is_ok()).then_some(url)
    }
}

impl StreamProvider for Anineko {
    fn name(&self) -> &'static str {
        "anineko"
    }

    fn display_name(&self) -> &'static str {
        "Anineko"
    }

    /// No canonical-keyed route exists: the slug only comes back from a title
    /// search. None sends every bind to the search path.
    fn canonical_key(&self, _show: &Enrichment) -> Option<String> {
        None
    }

    /// Site relevance order, preserved; the scorer chooses. No paging on this
    /// endpoint, so page 2 would repeat page 1.
    fn search(&self, query: &str, opts: &SearchOptions) -> Result<Vec<SearchHit>, ProviderError> {
        if opts.page > 1 || query.is_empty() {
            return Ok(Vec::new());
        }
        let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        let url = format!("{}/ajax/search?q={encoded}", self.host);
        let raw = self.ajax_get(&url)?;
        let mut hits = parse_search(&raw)?;
        hits.truncate(opts.limit as usize);
        Ok(hits)
    }

    /// Track-agnostic: the sub/dub fork lives on each episode page, so
    /// filtering here would cost one fetch per episode. A missing dub surfaces
    /// at resolve. `count_hint` unused: the show page is a real listing.
    fn episodes(
        &self,
        provider_id: &str,
        _tt: Translation,
        _count_hint: Option<u32>,
    ) -> Result<Vec<String>, ProviderError> {
        guard_slug(provider_id)?;
        let url = format!("{}/watch/{provider_id}", self.host);
        let html = self.page_get(&url, REFERER)?;
        let text = String::from_utf8_lossy(&html);
        let nums = parse_episode_numbers(&text, provider_id);
        // Never mints absence. `canonical_key` is always None, so this is only
        // reached with a scorer-accepted slug or one a prior listing minted;
        // neither parses to zero legitimately, and a delisting 404s. Ok(vec![])
        // here would stamp a 7-day not-stocked TTL off a lie (03 §4.3).
        if nums.is_empty() {
            return Err(ProviderError::Decode("no episodes on page".into()));
        }
        Ok(nums.into_iter().map(|n| n.to_string()).collect())
    }

    fn resolve(
        &self,
        provider_id: &str,
        episode: &str,
        tt: Translation,
        quality: Quality,
    ) -> Result<StreamLink, ProviderError> {
        guard_slug(provider_id)?;
        // Splice the parsed `n` back, not the raw label: parse accepts "+1" and
        // "007", so the canonical form is what reaches the URL.
        let n: u32 = episode
            .parse()
            .map_err(|_| ProviderError::Decode("invalid episode".into()))?;
        if n == 0 {
            return Err(ProviderError::Decode("invalid episode".into()));
        }

        let page_url = format!("{}/watch/{provider_id}/ep-{n}", self.host);
        let html = self.page_get(&page_url, REFERER)?;
        let text = String::from_utf8_lossy(&html);
        let servers: Vec<Server> = parse_servers(&text)
            .into_iter()
            .filter(|s| s.track == tt)
            .collect();
        if servers.is_empty() {
            // Past-end, or the track is not stocked. Either way nothing to play.
            return Err(ProviderError::Decode("no stream for track".into()));
        }

        // Vet before taking the budget, or malformed leading entries spend the
        // allowance and strand the working hosts behind them.
        for server in servers
            .iter()
            .filter(|s| {
                is_absolute_url(&s.url) && clean_arg(&s.url) && guard_fetch_url(&s.url).is_ok()
            })
            .take(MAX_EMBED_TRIES)
        {
            let embed_referer = format!("{}/", origin_of(&server.url));
            let embed = match self.page_get(&server.url, REFERER) {
                Ok(body) => String::from_utf8_lossy(&body).into_owned(),
                Err(_) => continue,
            };
            let Some(master) = embed_master(&embed) else {
                continue;
            };

            let url = self
                .cap_variant(master, &embed_referer, quality)
                .unwrap_or_else(|| master.to_string());
            let sub_url = if tt == Translation::Sub {
                self.guarded_subtitle(&server.url)
            } else {
                None
            };
            return Ok(StreamLink {
                url,
                resolution: None,
                referer: Some(embed_referer),
                user_agent: Some(UA.to_string()),
                // Every segment is a 1x1 PNG header with TS appended, served off
                // a shared ad CDN: mpv needs the relaxed demuxer and the proxy.
                cloaked_segments: true,
                decloak_segments: true,
                sub_url,
            });
        }
        Err(ProviderError::Decode("no playable source".into()))
    }

    /// No covers of its own beyond the ones search already hands back as
    /// absolute CDN refs; a relative or unsafe ref is refused, never
    /// "sanitized" and fetched.
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

/// Scheme + authority of an absolute URL, or the whole string when it does not
/// parse (the caller has already vetted it as absolute).
fn origin_of(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => parsed.origin().ascii_serialization(),
        Err(_) => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_slug_accepts_real_slugs_rejects_traversal_and_query() {
        assert!(guard_slug("frieren-beyond-journeys-end").is_ok());
        assert!(guard_slug("sousou_no_frieren_2").is_ok());
        assert!(guard_slug("re-zero-season-2").is_ok());
        for bad in [
            "",
            "../etc",
            "slug/extra",
            "slug?q=1",
            "slug#frag",
            "slug with space",
            "slug.json",
            "sl%2fug",
        ] {
            assert!(guard_slug(bad).is_err(), "slug {bad:?} must be refused");
        }
        assert!(guard_slug(&"x".repeat(MAX_SLUG_LEN + 1)).is_err());
    }

    #[test]
    fn slug_from_url_takes_watch_paths_only() {
        assert_eq!(
            slug_from_url("/watch/frieren-beyond-journeys-end"),
            Some("frieren-beyond-journeys-end")
        );
        assert_eq!(slug_from_url("watch/x-1"), Some("x-1"));
        assert_eq!(slug_from_url("/watch/x-1/"), Some("x-1"));
        // An episode-deep url is a shape change, not a show id: refuse rather
        // than truncate to the show.
        assert_eq!(slug_from_url("/watch/x-1/ep-3"), None);
        assert_eq!(slug_from_url("/anime/x-1"), None);
        assert_eq!(slug_from_url("/watch/"), None);
        assert_eq!(slug_from_url(""), None);
    }

    #[test]
    fn meta_episodes_reads_the_count_and_degrades_to_unknown() {
        assert_eq!(meta_episodes("TV \u{2022} 28 Episodes"), Some(28));
        assert_eq!(meta_episodes("Movie \u{2022} 1 Episodes"), Some(1));
        assert_eq!(meta_episodes("ONA \u{2022} 23 Episodes"), Some(23));
        assert_eq!(meta_episodes("Special \u{2022} 1 Episodes"), Some(1));
        // A countless meta must read as unknown, never as zero episodes.
        assert_eq!(meta_episodes("TV"), None);
        assert_eq!(meta_episodes("TV \u{2022} Unknown Episodes"), None);
        assert_eq!(meta_episodes("TV \u{2022} 0 Episodes"), None);
        assert_eq!(meta_episodes(""), None);
        // The same slot carries a bare YEAR: the site lists One Piece as
        // "TV • 1999". Read as a count it invents a ~900 episode gap and the
        // scorer rejects the correct show.
        assert_eq!(meta_episodes("TV \u{2022} 1999"), None);
        assert_eq!(meta_episodes("Movie \u{2022} 2016"), None);
    }

    const SEARCH_FIXTURE: &str = r#"{"success":true,"results":[
        {"title":"Frieren: Beyond Journey's End","url":"/watch/frieren-beyond-journeys-end",
         "image":"https://cdn.test/cover/a.webp","meta":"TV • 28 Episodes"},
        {"title":"Frieren: Beyond Journey's End Season 2","url":"/watch/frieren-beyond-journeys-end-season-2",
         "image":"https://cdn.test/cover/b.webp","meta":"TV • 10 Episodes"},
        {"title":"No Count","url":"/watch/no-count","meta":"TV"},
        {"title":"Bad Url","url":"/anime/nope","meta":"TV • 3 Episodes"},
        {"url":"/watch/titleless","meta":"TV • 3 Episodes"}
    ]}"#;

    #[test]
    fn parse_search_maps_hits_and_drops_unusable_rows() {
        let hits = parse_search(SEARCH_FIXTURE.as_bytes()).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].provider_id, "frieren-beyond-journeys-end");
        assert_eq!(hits[0].title, "Frieren: Beyond Journey's End");
        assert_eq!(hits[0].total_episodes, Some(28));
        assert_eq!(hits[1].total_episodes, Some(10));
        assert_eq!(hits[2].total_episodes, None);
        // The payload has no ids and no year; leaving them Some would fake
        // corroboration the site never gave.
        assert!(hits.iter().all(|h| h.anilist_id.is_none()));
        assert!(hits.iter().all(|h| h.mal_id.is_none()));
        assert!(hits.iter().all(|h| h.year.is_none()));
    }

    #[test]
    fn parse_search_empty_and_malformed() {
        assert!(
            parse_search(br#"{"success":true,"results":[]}"#)
                .unwrap()
                .is_empty()
        );
        assert!(parse_search(br#"{"success":false}"#).unwrap().is_empty());
        assert!(matches!(
            parse_search(b"<html>nope</html>"),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn parse_episode_numbers_sorted_deduped_and_slug_scoped() {
        let html = r#"
            <a href="/watch/show-a/ep-1">1</a>
            <a href="/watch/show-a/ep-3">3</a>
            <a href="/watch/show-a/ep-2">2</a>
            <a href="/watch/show-a/ep-2">dup</a>
            <a href="/watch/other-show/ep-9">sidebar</a>
        "#;
        assert_eq!(parse_episode_numbers(html, "show-a"), vec![1, 2, 3]);
        assert!(parse_episode_numbers(html, "missing-show").is_empty());
        assert!(parse_episode_numbers("<html>no eps</html>", "show-a").is_empty());
    }

    #[test]
    fn parse_episode_numbers_survives_quote_and_href_style_drift() {
        // Each of these is valid HTML the site could serve tomorrow. Under the
        // old `"/watch/` anchor every one scanned as zero episodes, and zero
        // episodes is a persisted not-stocked verdict, so markup drift alone
        // could blacklist a show. Retrieval must not depend on quote style.
        for html in [
            r#"<a href="/watch/show-a/ep-1"></a><a href="/watch/show-a/ep-2"></a>"#,
            r#"<a href='/watch/show-a/ep-1'></a><a href='/watch/show-a/ep-2'></a>"#,
            r#"<a href="watch/show-a/ep-1"></a><a href="watch/show-a/ep-2"></a>"#,
            r#"<a href="https://anineko.to/watch/show-a/ep-1"></a><a href="https://anineko.to/watch/show-a/ep-2"></a>"#,
        ] {
            assert_eq!(
                parse_episode_numbers(html, "show-a"),
                vec![1, 2],
                "drifted markup must still list episodes: {html}"
            );
        }
    }

    #[test]
    fn parse_episode_numbers_does_not_match_a_sibling_slug() {
        // `/ep-` is what disambiguates: the byte after the slug must be the
        // slash, so a longer sibling slug cannot bleed into this listing.
        let html = r#"<a href="/watch/show-a-2/ep-5"></a><a href="/watch/show-a/ep-1"></a>"#;
        assert_eq!(parse_episode_numbers(html, "show-a"), vec![1]);
    }

    #[test]
    fn parse_episode_numbers_clamps_a_hostile_listing() {
        // Every other episode-minting path clamps to MAX_EPISODE_HINT; an
        // unbounded listing would push the whole set into the episode cache
        // and the grid.
        let flood: String = (1..=MAX_EPISODE_HINT + 500)
            .map(|n| format!(r#"<a href="/watch/s/ep-{n}"></a>"#))
            .collect();
        assert_eq!(
            parse_episode_numbers(&flood, "s").len(),
            MAX_EPISODE_HINT as usize
        );
    }

    #[test]
    fn parse_episode_numbers_ignores_zero_and_non_numeric() {
        let html = r#"<a href="/watch/s/ep-0"></a><a href="/watch/s/ep-x"></a><a href="/watch/s/ep-7"></a>"#;
        assert_eq!(parse_episode_numbers(html, "s"), vec![7]);
    }

    const EPISODE_FIXTURE: &str = r#"
        <button class="server default" data-video="https://vivi.test/aaa?sub=https://cdn.test/eng.vtt" data-tab="tab_0"> HD-1 <span>Sort Sub</span> </button>
        <button class="server" data-video="https://hg.test/e/bbb?caption_1=https://cdn.test/eng.vtt&sub_1=English" data-tab="tab_0"> StreamHG <span>Sort Sub</span> </button>
        <button class="server default" data-video="https://vivi.test/ccc" data-tab="tab_1"> HD-1 <span>DUB</span> </button>
        <button class="server" data-video="https://mogo.test/e/ddd?c1_file=https://cdn.test/eng.vtt&c1_label=English" data-tab="tab_1"> Doodstream <span>DUB</span> </button>
    "#;

    #[test]
    fn parse_servers_reads_page_order_and_the_forward_track_label() {
        let servers = parse_servers(EPISODE_FIXTURE);
        assert_eq!(servers.len(), 4);
        assert_eq!(
            servers[0].url,
            "https://vivi.test/aaa?sub=https://cdn.test/eng.vtt"
        );
        // "Sort Sub" is the live sub label; reading it as a dub would swap every
        // track on the site.
        assert_eq!(servers[0].track, Translation::Sub);
        assert_eq!(servers[1].track, Translation::Sub);
        assert_eq!(servers[2].track, Translation::Dub);
        assert_eq!(servers[3].track, Translation::Dub);
        assert!(parse_servers("<html>no servers</html>").is_empty());
    }

    #[test]
    fn parse_servers_stops_at_an_unclosed_button() {
        // Falling back to end-of-document would classify the entry against the
        // whole page, so a stray "dub" in a footer flips its track and a sub
        // request then reports no stream. Unbalanced markup is not parseable.
        let html = r#"<button data-video="https://vivi.test/aaa"> HD-1 <span>Sort Sub</span>
                      <footer>dubbed releases</footer>"#;
        assert!(parse_servers(html).is_empty());
    }

    #[test]
    fn track_of_classifies_the_button_label() {
        assert_eq!(track_of(" HD-1 <span>Sort Sub</span> "), Translation::Sub);
        assert_eq!(track_of(" HD-1 <span>DUB</span> "), Translation::Dub);
        assert_eq!(track_of(" HD-1 <span>Dubbed</span> "), Translation::Dub);
        assert_eq!(track_of(""), Translation::Sub);
    }

    #[test]
    fn parse_embed_src_reads_the_cleartext_master() {
        assert_eq!(
            parse_embed_src(
                r#"<script>const src = "https://vivi.test/public/stream/x/master.m3u8";</script>"#
            ),
            Some("https://vivi.test/public/stream/x/master.m3u8")
        );
        assert_eq!(
            parse_embed_src("const src='https://cdn.test/m.m3u8'"),
            Some("https://cdn.test/m.m3u8")
        );
        assert_eq!(parse_embed_src(r#"const src = """#), None);
        assert_eq!(parse_embed_src("const src = notaquote"), None);
        assert_eq!(parse_embed_src("<html>no player</html>"), None);
    }

    #[test]
    fn embed_master_refuses_a_master_aimed_at_a_private_host() {
        // The vetting is folded into the extraction so it is reachable offline:
        // as a bare `if` in the resolve loop no test could observe it, because
        // every mock listener is on loopback and the embed guard fires first.
        let good = r#"const src = "https://vivi.test/public/stream/x/master.m3u8";"#;
        assert_eq!(
            embed_master(good),
            Some("https://vivi.test/public/stream/x/master.m3u8")
        );
        for bad in [
            r#"const src = "http://127.0.0.1:9/master.m3u8";"#,
            r#"const src = "http://169.254.169.254/latest/meta-data/";"#,
            r#"const src = "/relative/master.m3u8";"#,
        ] {
            assert_eq!(embed_master(bad), None, "{bad} must not become a play url");
        }
    }

    #[test]
    fn parse_embed_src_scans_past_a_longer_identifier() {
        // The needle is a prefix of `const srcSet` / `const source`; stopping
        // at the first near-miss would hide the real declaration below it.
        let html = r#"const srcSet = "https://wrong.test/a.jpg";
                      const src = "https://right.test/master.m3u8";"#;
        assert_eq!(
            parse_embed_src(html),
            Some("https://right.test/master.m3u8")
        );
    }

    #[test]
    fn subtitle_from_embed_url_finds_the_sidecar_under_any_param_name() {
        assert_eq!(
            subtitle_from_embed_url("https://vivi.test/aaa?sub=https://cdn.test/eng.vtt")
                .as_deref(),
            Some("https://cdn.test/eng.vtt")
        );
        assert_eq!(
            subtitle_from_embed_url(
                "https://hg.test/e/bbb?caption_1=https://cdn.test/eng.vtt&sub_1=English"
            )
            .as_deref(),
            Some("https://cdn.test/eng.vtt")
        );
        assert_eq!(
            subtitle_from_embed_url(
                "https://mogo.test/e/ddd?c1_file=https://cdn.test/eng.srt&c1_label=English"
            )
            .as_deref(),
            Some("https://cdn.test/eng.srt")
        );
        // The label param is not a file; a param-name whitelist would have to
        // grow per host, so the pick is value-shaped instead.
        assert_eq!(
            subtitle_from_embed_url("https://vivi.test/aaa?sub_1=English"),
            None
        );
        assert_eq!(subtitle_from_embed_url("https://vivi.test/aaa"), None);
        assert_eq!(subtitle_from_embed_url("not a url"), None);
    }

    #[test]
    fn has_subtitle_extension_tests_the_path_not_the_query() {
        assert!(has_subtitle_extension("https://cdn.test/a.vtt"));
        assert!(has_subtitle_extension("https://cdn.test/A.VTT"));
        assert!(has_subtitle_extension("https://cdn.test/a.ass"));
        // A query or fragment must not be able to smuggle the suffix in.
        assert!(!has_subtitle_extension("https://cdn.test/evil.exe?x=a.vtt"));
        assert!(!has_subtitle_extension("https://cdn.test/evil.exe#a.vtt"));
        assert!(!has_subtitle_extension("https://cdn.test/a.m3u8"));
    }

    #[test]
    fn origin_of_extracts_scheme_and_host() {
        assert_eq!(origin_of("https://vivi.test/a/b?x=1"), "https://vivi.test");
        assert_eq!(origin_of("http://cdn.test:8080/p"), "http://cdn.test:8080");
    }

    // -- provider surface (no network: guards fire before the wire) -----------

    #[test]
    fn canonical_key_is_none_because_the_site_has_no_canonical_route() {
        let p = Anineko::new().unwrap();
        let show = Enrichment {
            anilist_id: 154587,
            mal_id: Some(52991),
            ..Enrichment::default()
        };
        assert_eq!(p.canonical_key(&show), None);
        // canonical_key never answers, so search off would make the provider
        // unbindable.
        assert!(p.supports_search());
    }

    #[test]
    fn episodes_rejects_a_smuggled_slug_before_fetch() {
        let p = Anineko::new().unwrap();
        for bad in ["../7", "a/b", ""] {
            assert!(matches!(
                p.episodes(bad, Translation::Sub, None),
                Err(ProviderError::Decode(_))
            ));
        }
    }

    #[test]
    fn resolve_rejects_foreign_or_corrupt_episode_labels_before_any_network() {
        let p = Anineko::new().unwrap();
        for bad in ["13.5", "0", "", "abc"] {
            assert!(
                matches!(
                    p.resolve("show-a", bad, Translation::Sub, Quality::Best),
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
    fn guarded_subtitle_drops_a_sidecar_aimed_at_a_private_host() {
        let p = Anineko::new().unwrap();
        // sub_url bypasses the proxy and reaches mpv --sub-file directly.
        for host in [
            "http://169.254.169.254/latest/meta-data/x.vtt",
            "http://127.0.0.1:9/pwn.vtt",
        ] {
            assert_eq!(
                p.guarded_subtitle(&format!("https://vivi.test/a?sub={host}")),
                None,
                "{host} must be guarded out"
            );
        }
        assert_eq!(
            p.guarded_subtitle("https://vivi.test/a?sub=https://cdn.test/eng.vtt")
                .as_deref(),
            Some("https://cdn.test/eng.vtt")
        );
    }

    #[test]
    fn cover_request_absolute_passes_relative_and_unsafe_reject() {
        let p = Anineko::new().unwrap();
        let abs = p.cover_request("https://cdn.test/cover/a.webp").unwrap();
        assert_eq!(abs.url, "https://cdn.test/cover/a.webp");
        assert_eq!(abs.referer, None);
        for bad in ["/cover/a.webp", "", "https://cdn.test/a b.webp"] {
            assert!(p.cover_request(bad).is_err(), "ref {bad:?} must be refused");
        }
    }

    // -- transport (single-hop paths against the one-shot mock) --------------

    use crate::testutil::{response_with_body, serve_once};

    fn against(response: Vec<u8>) -> Anineko {
        Anineko::with_host(serve_once(response).trim_end_matches('/').to_string()).unwrap()
    }

    fn opts() -> SearchOptions {
        SearchOptions {
            translation: Translation::Sub,
            limit: 26,
            page: 1,
        }
    }

    #[test]
    fn transport_search_maps_the_payload() {
        let p = against(response_with_body("200 OK", SEARCH_FIXTURE.as_bytes()));
        let hits = p.search("frieren", &opts()).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].provider_id, "frieren-beyond-journeys-end");
    }

    #[test]
    fn search_page_two_is_empty_without_touching_the_wire() {
        // No paging on this endpoint: page 2 would repeat page 1 and the scorer
        // would see the same candidates twice.
        let p = Anineko::new().unwrap();
        let hits = p
            .search("frieren", &SearchOptions { page: 2, ..opts() })
            .unwrap();
        assert!(hits.is_empty());
        assert!(p.search("", &opts()).unwrap().is_empty());
    }

    #[test]
    fn transport_search_percent_encodes_the_query() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let body = response_with_body("200 OK", br#"{"results":[]}"#);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = std::io::Read::read(&mut sock, &mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = std::io::Write::write_all(&mut sock, &body);
        });
        let p = Anineko::with_host(format!("http://{addr}")).unwrap();
        let _ = p.search("fate/zero & co", &opts());
        let req = rx.recv().unwrap();
        assert!(
            req.contains("q=fate%2Fzero+%26+co"),
            "query must be encoded, got: {req}"
        );
    }

    #[test]
    fn transport_episodes_from_the_show_page() {
        let html = br#"<a href="/watch/show-a/ep-1">1</a><a href="/watch/show-a/ep-2">2</a>"#;
        let p = against(response_with_body("200 OK", html));
        let eps = p.episodes("show-a", Translation::Sub, None).unwrap();
        assert_eq!(eps, ["1", "2"]);
    }

    #[test]
    fn transport_episodes_forbidden_maps_to_taxonomy() {
        // A block must stay an Err: Ok(vec![]) would stamp a false absence.
        let p = against(response_with_body("403 Forbidden", b""));
        let got = p.episodes("show-a", Translation::Sub, None);
        assert!(matches!(got, Err(ProviderError::Forbidden { status: 403 })));
    }

    #[test]
    fn transport_resolve_reports_a_track_with_no_servers() {
        // The episode page lists sub only; a dub resolve must fail cleanly
        // rather than play the sub track. Match the PAYLOAD, not just the
        // variant: drop the track filter and this becomes "no playable source",
        // so a bare Decode(_) assertion passes with sub/dub selection gone.
        let page =
            br#"<button data-video="https://vivi.test/aaa"> HD-1 <span>Sort Sub</span> </button>"#;
        let p = against(response_with_body("200 OK", page));
        let got = p.resolve("show-a", "1", Translation::Dub, Quality::Best);
        assert!(
            matches!(&got, Err(ProviderError::Decode(m)) if m == "no stream for track"),
            "expected the track-filter refusal, got {got:?}"
        );
    }

    #[test]
    fn transport_resolve_never_fetches_a_private_embed_host() {
        // Bind a real listener and assert NOTHING connects. Asserting only on
        // the returned error cannot tell "skipped by the guard" from "fetched
        // and refused": both land on the same Decode, so that assertion passes
        // with the guard deleted.
        let trap = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let trap_addr = trap.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if trap.accept().is_ok() {
                let _ = tx.send(());
            }
        });
        let page = format!(
            r#"<button data-video="http://{trap_addr}/embed"> HD-1 <span>Sort Sub</span> </button>"#
        );
        let p = against(response_with_body("200 OK", page.as_bytes()));
        let got = p.resolve("show-a", "1", Translation::Sub, Quality::Best);
        assert!(matches!(got, Err(ProviderError::Decode(_))), "{got:?}");
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "the embed url was fetched; the SSRF guard did not fire"
        );
    }

    #[test]
    fn cap_variant_never_fetches_a_private_master_playlist() {
        // Exercised directly rather than through resolve: every test listener
        // is on loopback, so a resolve-level attempt is stopped by the EMBED
        // guard first and can never observe this one. cap_variant is the
        // chokepoint that stops the request leaving the process.
        let trap = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let trap_addr = trap.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            if trap.accept().is_ok() {
                let _ = tx.send(());
            }
        });
        let p = Anineko::new().unwrap();
        let got = p.cap_variant(
            &format!("http://{trap_addr}/master.m3u8"),
            "https://vivi.test/",
            Quality::Best,
        );
        assert_eq!(got, None);
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "the master playlist was fetched despite pointing at a private host"
        );
    }

    #[test]
    fn master_ok_refuses_what_must_never_become_a_play_url() {
        assert!(master_ok("https://vivi.test/public/stream/x/master.m3u8"));
        // Private and link-local: under a quality cap we fetch this ourselves,
        // and the fallback would otherwise hand it to the player.
        for bad in [
            "http://127.0.0.1:9/master.m3u8",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/master.m3u8",
            "/relative/master.m3u8",
            "https://vivi.test/a b.m3u8",
            "",
        ] {
            assert!(!master_ok(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn transport_episodes_404_errors_rather_than_stamping_absence() {
        // A dead slug 404s on this site. Ok(vec![]) would burn it in as a
        // permanent not-stocked verdict; a rename or a pull must stay
        // recoverable. Pinned offline so CI runs it, not only the live file.
        let p = against(response_with_body("404 Not Found", b""));
        let got = p.episodes("gone-show", Translation::Sub, None);
        assert!(matches!(got, Err(ProviderError::Http { status: 404 })));
    }

    #[test]
    fn transport_episodes_never_stamps_absence_from_a_parsed_nothing_page() {
        // Every 200 that parses to nothing is a lie (app shell, challenge,
        // markup drift), and Ok(vec![]) here would persist as a permanent
        // not-stocked verdict. This provider has no path that mints absence.
        for body in [
            &b"<html><div id=\"app\"></div></html>"[..],
            b"<html>window._cf_chl_opt={cvId:'3'}</html>",
            br#"<html><link rel="canonical" href="https://anineko.to/watch/show-a"></html>"#,
        ] {
            let p = against(response_with_body("200 OK", body));
            let got = p.episodes("show-a", Translation::Sub, None);
            assert!(
                matches!(got, Err(ProviderError::Decode(_))),
                "a parsed-nothing page must not stamp absence, got {got:?}"
            );
        }
    }
}
