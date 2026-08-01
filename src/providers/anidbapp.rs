//! anidb.app `StreamProvider` (ROD-516). Seam tier C, confidence tier A: the
//! site exposes no AniList-keyed endpoint, so `canonical_key` cannot answer.
//! Binding runs through title search, and each candidate carries the AniList
//! and MAL ids scraped off its detail page, which `resolver::best_id_match`
//! confirms before any fuzzy title scoring ever runs.
//!
//! Chain: /search/suggestions (HTML cards) -> /anime/{slug} (external ids) ->
//! /api/frontend/anime/{siteId}/episodes -> /api/frontend/episode/{id}/languages
//! -> embed HTML -> jwplayer HLS master. A default UA gets 403; a browser UA
//! passes every surface, and no endpoint carries a session gate.
//!
//! The edge also scores HTTP/1.1 header casing against that UA, which is why
//! the reqwest `http2` feature is mandatory (see Cargo.toml). Drop it and every
//! request here answers a challenge page.

use serde::Deserialize;

use super::http::{Accept, HttpClient, Method, Request};
use crate::domain::{Enrichment, Quality, StreamLink, Translation, is_absolute_url};
use crate::fetchguard::guard_fetch_url;
use crate::providers::{
    CoverRequest, MAX_COVER_REF_LEN, ProviderError, SearchHit, SearchOptions, StreamProvider,
    clean_arg, guard_show_id,
};

const API: &str = "https://anidb.app";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const REFERER: &str = "https://anidb.app/";
/// Detail-page probes per search. The suggestions endpoint returns at most 8
/// cards, so in practice every candidate reaches the scorer id-annotated.
const MAX_PROBE: usize = 8;
/// The endpoint answers nothing below this.
const MIN_QUERY_LEN: usize = 2;
/// Audio codes. `jpn` carries burned-in subs; there is no sidecar track.
const CODE_SUB: &str = "jpn";
const CODE_DUB: &str = "eng";

// -- DTOs -------------------------------------------------------------------

#[derive(Deserialize)]
struct EpisodesResp {
    #[serde(default)]
    episodes: Vec<EpisodeRow>,
}

#[derive(Deserialize)]
struct EpisodeRow {
    id: Option<i64>,
    number: Option<i64>,
}

#[derive(Deserialize)]
struct LanguagesResp {
    #[serde(default)]
    languages: Vec<LanguageRow>,
}

#[derive(Deserialize)]
struct LanguageRow {
    code: Option<String>,
    embed_url: Option<String>,
}

#[derive(Debug, PartialEq)]
struct Episode {
    id: i64,
    number: u32,
}

#[derive(Debug, PartialEq)]
struct Card {
    slug: String,
    site_id: String,
    title: String,
    year: Option<u32>,
}

// -- episode listing --------------------------------------------------------

/// Rows to episodes, ascending by number, one row per number.
fn parse_episodes(raw: &[u8]) -> Result<Vec<Episode>, ProviderError> {
    let resp: EpisodesResp =
        serde_json::from_slice(raw).map_err(|e| ProviderError::Decode(format!("episodes: {e}")))?;
    let mut eps: Vec<Episode> = resp
        .episodes
        .into_iter()
        .filter_map(|r| {
            let id = r.id.filter(|&i| i > 0)?;
            let number = r
                .number
                .filter(|&n| (1..=i64::from(u32::MAX)).contains(&n))?;
            Some(Episode {
                id,
                number: number as u32,
            })
        })
        .collect();
    // Stable: dedup keeps the first of a same-numbered pair, so which row wins
    // has to be the one the site listed first, not whichever the sort landed.
    eps.sort_by_key(|e| e.number);
    eps.dedup_by_key(|e| e.number);
    Ok(eps)
}

/// Distance between the site's numbering and the canonical's.
///
/// A season the site numbers franchise-absolute (Frieren S2 runs 29..38) has
/// an AniList entry numbering 1..10, and the binding points at that entry.
/// Labels are the canonical's numbers; the offset converts back on resolve.
fn base_offset(eps: &[Episode]) -> u32 {
    eps.first().map_or(0, |e| e.number.saturating_sub(1))
}

fn label(ep: &Episode, offset: u32) -> String {
    ep.number.saturating_sub(offset).to_string()
}

// -- search card parsing ----------------------------------------------------

/// Suggestion cards from the search HTML. Each is an `<a>` marked
/// `data-search-item` wrapping a poster and two `<p>`: the title and a
/// `TYPE · YEAR` line.
fn parse_cards(html: &str) -> Vec<Card> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("<a ") {
        let after = &rest[at + "<a ".len()..];
        let (block, next) = match after.find("</a>") {
            Some(end) => (&after[..end], &after[end + "</a>".len()..]),
            None => (after, ""),
        };
        rest = next;
        if !block.contains("data-search-item") {
            continue;
        }
        if let Some(card) = parse_card(block) {
            out.push(card);
        }
    }
    out
}

fn parse_card(block: &str) -> Option<Card> {
    let href = attr_value(block, "href")?;
    let (slug, site_id) = split_slug(&href)?;
    let texts = tag_texts(block, "p");
    let title = decode_entities(texts.first()?.trim());
    if title.is_empty() {
        return None;
    }
    Some(Card {
        slug,
        site_id,
        title,
        year: texts.get(1).and_then(|meta| trailing_year(meta)),
    })
}

/// Slug and trailing site id from a card href.
///
/// Only the last path segment is kept and the host is a constant, so a forged
/// href cannot move the probe off our origin. The charset guard is narrower
/// than that: it rejects a segment that would corrupt the URL we build from it.
fn split_slug(href: &str) -> Option<(String, String)> {
    if !href.contains("/anime/") {
        return None;
    }
    let slug = href.rsplit('/').next()?;
    let slug = slug.split(['?', '#']).next()?;
    if slug.is_empty()
        || !slug
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return None;
    }
    let (name, id) = slug.rsplit_once('-')?;
    if name.is_empty() || id.is_empty() || !id.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((slug.to_string(), id.to_string()))
}

fn attr_value(block: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let at = block.find(&needle)? + needle.len();
    let rest = &block[at..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Inner text of every `<tag ...>...</tag>` in `block`, outermost-first.
fn tag_texts(block: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut out = Vec::new();
    let mut rest = block;
    while let Some(at) = rest.find(&open) {
        let after = &rest[at + open.len()..];
        let Some(gt) = after.find('>') else { break };
        let inner = &after[gt + 1..];
        let Some(end) = inner.find(&close) else { break };
        out.push(inner[..end].to_string());
        rest = &inner[end + close.len()..];
    }
    out
}

/// Last 4-digit run in the `TYPE · YEAR` line.
fn trailing_year(meta: &str) -> Option<u32> {
    let bytes = meta.as_bytes();
    let mut found = None;
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i - start == 4 {
            found = meta[start..i].parse().ok();
        }
    }
    found
}

/// Single-pass HTML entity decode. Card titles arrive escaped and feed the
/// fuzzy title scorer, so `Journey&#039;s` must reach it as an apostrophe.
/// Unknown entities stay literal.
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        match tail.find(';').filter(|&end| end <= 10) {
            Some(end) => {
                match entity_char(&tail[1..end]) {
                    Some(c) => out.push(c),
                    None => out.push_str(&tail[..=end]),
                }
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity_char(entity: &str) -> Option<char> {
    match entity {
        "amp" => return Some('&'),
        "lt" => return Some('<'),
        "gt" => return Some('>'),
        "quot" => return Some('"'),
        "apos" => return Some('\''),
        "nbsp" => return Some(' '),
        _ => {}
    }
    let digits = entity.strip_prefix('#')?;
    let code = match digits.strip_prefix(['x', 'X']) {
        Some(hex) => u32::from_str_radix(hex, 16).ok()?,
        None => digits.parse().ok()?,
    };
    char::from_u32(code)
}

// -- detail page + embed ----------------------------------------------------

/// AniList and MAL ids from the detail page's external-link block.
fn parse_external_ids(html: &str) -> (Option<i64>, Option<i64>) {
    (
        id_after(html, "anilist.co/anime/"),
        id_after(html, "myanimelist.net/anime/"),
    )
}

fn id_after(html: &str, marker: &str) -> Option<i64> {
    let at = html.find(marker)? + marker.len();
    let rest = &html[at..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Cloudflare interstitial served at 200, where an empty parse would otherwise
/// pose as "no results" and stamp a 7-day absence.
///
/// Two markers that look usable are not. `/cdn-cgi/challenge-platform` ships on
/// good embed pages, so keying on it would fail every playback. "Just a moment"
/// is ordinary loading copy anywhere in a body; only the interstitial puts it in
/// the title, so match it there or a synopsis could take the provider down.
fn is_challenge(html: &str) -> bool {
    html.contains("cf_chl_opt")
        || html.contains("__cf_chl")
        || tag_texts(html, "title")
            .iter()
            .any(|t| t.contains("Just a moment"))
}

/// HLS master out of the jwplayer setup.
///
/// The `sources` `file:` value first, then any quoted `.m3u8` as a fallback so
/// a config rename does not break extraction. Order is the point: the scan
/// takes the FIRST `.m3u8` on the page, which need not be the real source if
/// anything else on it (an ad slot, a preview thumbnail) carries one earlier.
fn extract_hls(html: &str) -> Option<String> {
    let url = config_value(html, "file")
        .filter(|u| u.contains(".m3u8"))
        .or_else(|| first_quoted_m3u8(html))?;
    url.starts_with("http").then_some(url)
}

/// Quoted value of `key: "..."` / `key: '...'`.
fn config_value(html: &str, key: &str) -> Option<String> {
    let at = html.find(key)?;
    let rest = html[at + key.len()..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let quote = *rest.as_bytes().first()?;
    if quote != b'"' && quote != b'\'' {
        return None;
    }
    let inner = &rest[1..];
    let end = inner.find(quote as char)?;
    Some(inner[..end].to_string())
}

/// First `.m3u8` on the page, widened to its enclosing quotes.
fn first_quoted_m3u8(html: &str) -> Option<String> {
    let at = html.find(".m3u8")?;
    let start = html[..at].rfind(['"', '\''])? + 1;
    let end = at + html[at..].find(['"', '\''])?;
    Some(html[start..end].to_string())
}

/// Vet a scraped stream url before it becomes the play url.
///
/// Scraped off the page, so it is untrusted twice over: under a quality cap we
/// fetch it ourselves, and either way it leaves as the play url. `cap_variant`
/// falls back to the raw value when its own guard refuses, so the SSRF check
/// has to happen here too, not only inside it. Pure, so it is unit-testable:
/// the transport path cannot reach it, because the embed fetch that precedes it
/// is itself guarded.
fn stream_url_ok(url: &str) -> bool {
    is_absolute_url(url) && clean_arg(url) && guard_fetch_url(url).is_ok()
}

// -- provider ---------------------------------------------------------------

pub struct AniDbApp {
    http: HttpClient,
    api: String,
}

impl AniDbApp {
    pub fn new() -> Result<AniDbApp, ProviderError> {
        AniDbApp::with_endpoint(API.to_string())
    }

    fn with_endpoint(api: String) -> Result<AniDbApp, ProviderError> {
        Ok(AniDbApp {
            http: HttpClient::new()?,
            api,
        })
    }

    fn json_get(&self, url: &str) -> Result<Vec<u8>, ProviderError> {
        self.http.fetch(&Request {
            method: Method::Get,
            url,
            payload: None,
            user_agent: UA,
            extra_headers: &[("Referer", REFERER), ("Accept", "application/json")],
            accept: Accept::Any2xx,
            deadline: None,
        })
    }

    /// GET an HTML surface, refusing a challenge interstitial as a block so the
    /// walk hops instead of reading it as an answer.
    fn page_get(&self, url: &str) -> Result<String, ProviderError> {
        let raw = self.http.fetch(&Request {
            method: Method::Get,
            url,
            payload: None,
            user_agent: UA,
            extra_headers: &[
                ("Referer", REFERER),
                (
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
                ),
                ("Accept-Language", "en-US,en;q=0.9"),
            ],
            accept: Accept::Any2xx,
            deadline: None,
        })?;
        let html = String::from_utf8_lossy(&raw).into_owned();
        if is_challenge(&html) {
            log::warn!("anidbapp: challenge page, treating as blocked");
            return Err(ProviderError::Forbidden { status: 403 });
        }
        Ok(html)
    }

    fn fetch_episodes(&self, site_id: &str) -> Result<Vec<Episode>, ProviderError> {
        let url = format!("{}/api/frontend/anime/{site_id}/episodes", self.api);
        parse_episodes(&self.json_get(&url)?)
    }

    fn fetch_languages(&self, ep_id: i64) -> Result<Vec<LanguageRow>, ProviderError> {
        let url = format!("{}/api/frontend/episode/{ep_id}/languages", self.api);
        let raw = self.json_get(&url)?;
        let resp: LanguagesResp = serde_json::from_slice(&raw)
            .map_err(|e| ProviderError::Decode(format!("languages: {e}")))?;
        Ok(resp.languages)
    }

    fn has_dub(&self, ep_id: i64) -> Result<bool, ProviderError> {
        Ok(self
            .fetch_languages(ep_id)?
            .iter()
            .any(|l| l.code.as_deref() == Some(CODE_DUB)))
    }

    /// Index of the last dubbed episode, or None when the show has no dub.
    ///
    /// Rests on dub availability being a prefix: dubs lag the sub release, they
    /// do not perforate it. Both ends dubbed is therefore taken as the whole run
    /// dubbed, without probing the interior.
    ///
    /// A perforated show breaks that both ways. A hole below the boundary
    /// over-reports, which lands softly: resolve returns a clean miss for the
    /// phantom episode. A hole ON a probe point drags the boundary down and
    /// under-reports, which is the quiet one, since real dubbed episodes just
    /// stop being listed. The alternative is a language call per episode, which
    /// a 1000-episode show cannot afford.
    fn dub_prefix(&self, eps: &[Episode]) -> Result<Option<usize>, ProviderError> {
        let Some(last) = eps.len().checked_sub(1) else {
            return Ok(None);
        };
        if !self.has_dub(eps[0].id)? {
            return Ok(None);
        }
        if last == 0 || self.has_dub(eps[last].id)? {
            return Ok(Some(last));
        }
        let (mut lo, mut hi) = (0usize, last);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if self.has_dub(eps[mid].id)? {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Ok(Some(lo))
    }

    fn probe_ids(&self, slug: &str) -> Result<(Option<i64>, Option<i64>), ProviderError> {
        let url = format!("{}/anime/{slug}", self.api);
        Ok(parse_external_ids(&self.page_get(&url)?))
    }

    /// Variant matching the quality cap, or None so resolve keeps the master.
    fn cap_variant(&self, master_url: &str, quality: Quality) -> Option<String> {
        guard_fetch_url(master_url).ok()?;
        let body = self
            .http
            .fetch(&Request {
                method: Method::Get,
                url: master_url,
                payload: None,
                user_agent: UA,
                extra_headers: &[("Referer", REFERER)],
                accept: Accept::OkOnly,
                deadline: None,
            })
            .ok()?;
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
                    ..StreamLink::default()
                });
            }
        }
        let pick = super::hls::select_variant(&links, quality)?;
        log::debug!(
            "anidbapp resolve: quality={quality:?} picked {}p",
            pick.resolution
                .map_or_else(|| "?".to_string(), |r| r.to_string())
        );
        Some(pick.url.clone())
    }
}

impl StreamProvider for AniDbApp {
    fn name(&self) -> &'static str {
        "anidbapp"
    }

    fn display_name(&self) -> &'static str {
        "AniDB"
    }

    /// No canonical-keyed endpoint exists: the site id only comes back from a
    /// title search. None sends the walk to tier C, where the detail-page ids
    /// give the match tier-A confidence.
    fn canonical_key(&self, _show: &Enrichment) -> Option<String> {
        None
    }

    /// Suggestion cards, each probed for its AniList/MAL ids. Trust the site's
    /// relevance order; the scorer does the choosing.
    fn search(&self, query: &str, opts: &SearchOptions) -> Result<Vec<SearchHit>, ProviderError> {
        // No paging on this endpoint: page 2 would repeat page 1.
        if opts.page > 1 || query.chars().count() < MIN_QUERY_LEN {
            return Ok(Vec::new());
        }
        let encoded: String = url::form_urlencoded::byte_serialize(query.as_bytes()).collect();
        let url = format!("{}/search/suggestions?q={encoded}", self.api);
        let html = self.page_get(&url)?;

        let mut hits = Vec::new();
        for card in parse_cards(&html).into_iter().take(MAX_PROBE) {
            let (anilist_id, mal_id) = match self.probe_ids(&card.slug) {
                Ok(ids) => ids,
                // A block is provider-wide, so stop and let the walk hop. Any
                // other failure costs this one card its ids, not the search:
                // it can still bind on title.
                Err(e @ ProviderError::Forbidden { .. }) => return Err(e),
                Err(_) => (None, None),
            };
            hits.push(SearchHit {
                provider_id: card.site_id,
                title: card.title,
                anilist_id,
                mal_id,
                year: card.year,
                ..SearchHit::default()
            });
        }
        hits.truncate(opts.limit as usize);
        Ok(hits)
    }

    /// Sub lists every episode: `jpn` rides all of them, and confirming a
    /// universal would cost a language call per listing. Dub is bisected, and
    /// an empty result is per-track absence the way allanime already reports
    /// it.
    fn episodes(
        &self,
        provider_id: &str,
        tt: Translation,
        _count_hint: Option<u32>,
    ) -> Result<Vec<String>, ProviderError> {
        guard_show_id(provider_id)?;
        let eps = self.fetch_episodes(provider_id)?;
        let offset = base_offset(&eps);
        let listed: &[Episode] = match tt {
            Translation::Sub => &eps,
            Translation::Dub => match self.dub_prefix(&eps)? {
                Some(last) => &eps[..=last],
                None => &[],
            },
        };
        Ok(listed.iter().map(|e| label(e, offset)).collect())
    }

    fn resolve(
        &self,
        provider_id: &str,
        episode: &str,
        tt: Translation,
        quality: Quality,
    ) -> Result<StreamLink, ProviderError> {
        guard_show_id(provider_id)?;
        let want: u32 = episode
            .parse()
            .map_err(|_| ProviderError::Decode("invalid episode".into()))?;
        if want == 0 {
            return Err(ProviderError::Decode("invalid episode".into()));
        }

        let eps = self.fetch_episodes(provider_id)?;
        let offset = base_offset(&eps);
        let site_number = want
            .checked_add(offset)
            .ok_or_else(|| ProviderError::Decode("invalid episode".into()))?;
        let ep = eps
            .iter()
            .find(|e| e.number == site_number)
            .ok_or_else(|| ProviderError::Decode("no such episode".into()))?;

        let want_code = match tt {
            Translation::Sub => CODE_SUB,
            Translation::Dub => CODE_DUB,
        };
        let embed = self
            .fetch_languages(ep.id)?
            .into_iter()
            .find(|l| l.code.as_deref() == Some(want_code))
            .and_then(|l| l.embed_url)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| ProviderError::Decode("no stream for track".into()))?;
        if !is_absolute_url(&embed) || !clean_arg(&embed) || guard_fetch_url(&embed).is_err() {
            return Err(ProviderError::Decode("blocked embed url".into()));
        }

        let html = self.page_get(&embed)?;
        let master =
            extract_hls(&html).ok_or_else(|| ProviderError::Decode("no playable source".into()))?;
        if !stream_url_ok(&master) {
            return Err(ProviderError::Decode("bad stream url".into()));
        }

        // `best` leaves mpv on the master ladder; a cap fetches the variants.
        let chosen = if quality == Quality::Best {
            master.clone()
        } else {
            self.cap_variant(&master, quality).unwrap_or(master)
        };
        Ok(StreamLink {
            url: chosen,
            resolution: None,
            referer: Some(REFERER.to_string()),
            user_agent: Some(UA.to_string()),
            // Segments are plain TS named .xls and served as a spreadsheet
            // type; mpv must relax its demuxer gate. No decoy prefix, so the
            // stripping proxy stays out of it.
            cloaked_segments: true,
            decloak_segments: false,
            sub_url: None,
        })
    }

    fn cover_request(&self, cover_ref: &str) -> Result<CoverRequest, ProviderError> {
        if cover_ref.is_empty()
            || cover_ref.len() > MAX_COVER_REF_LEN
            || !is_absolute_url(cover_ref)
            || !clean_arg(cover_ref)
        {
            return Err(ProviderError::Decode("invalid cover ref".into()));
        }
        Ok(CoverRequest {
            url: cover_ref.to_string(),
            referer: Some(REFERER.to_string()),
            user_agent: Some(UA.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::response_with_body;
    use std::sync::{Arc, Mutex};

    // -- pure parsers -------------------------------------------------------

    const CARDS: &str = r#"
<a href="https://anidb.app/anime/frieren-beyond-journeys-end-season-2-1665"
   data-search-item
   class="flex items-center gap-3">
    <img src="https://cdn.test/1665.jpg" alt="Frieren: Beyond Journey&amp;#039;s End Season 2" class="w-9">
    <div class="min-w-0">
        <p class="text-sm">Frieren: Beyond Journey&#039;s End Season 2</p>
        <p class="text-xs">TV · 2026</p>
    </div>
</a>
<a href="https://anidb.app/anime/frieren-beyond-journeys-end-1663"
   data-search-item
   class="flex items-center gap-3">
    <img src="https://cdn.test/1663.jpg" alt="Frieren" class="w-9">
    <div class="min-w-0">
        <p class="text-sm">Frieren: Beyond Journey&#039;s End</p>
        <p class="text-xs">TV · 2023</p>
    </div>
</a>
"#;

    #[test]
    fn parse_cards_reads_slug_id_title_and_year() {
        let cards = parse_cards(CARDS);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[0].site_id, "1665");
        assert_eq!(cards[0].slug, "frieren-beyond-journeys-end-season-2-1665");
        assert_eq!(cards[0].title, "Frieren: Beyond Journey's End Season 2");
        assert_eq!(cards[0].year, Some(2026));
        assert_eq!(cards[1].site_id, "1663");
        assert_eq!(cards[1].title, "Frieren: Beyond Journey's End");
        assert_eq!(cards[1].year, Some(2023));
    }

    #[test]
    fn parse_cards_ignores_anchors_without_the_marker() {
        let html = r#"<a href="https://anidb.app/anime/other-99" class="nav"><p>Other</p></a>"#;
        assert!(parse_cards(html).is_empty());
        assert!(parse_cards("<html>no results</html>").is_empty());
    }

    #[test]
    fn parse_cards_drops_a_card_with_no_title() {
        let html = r#"<a href="https://anidb.app/anime/x-12" data-search-item><img src="x"></a>"#;
        assert!(parse_cards(html).is_empty());
    }

    #[test]
    fn split_slug_takes_the_trailing_id() {
        assert_eq!(
            split_slug("https://anidb.app/anime/one-piece-3880"),
            Some(("one-piece-3880".into(), "3880".into()))
        );
        assert_eq!(
            split_slug("/anime/x_y-7?ref=a"),
            Some(("x_y-7".into(), "7".into()))
        );
    }

    #[test]
    fn split_slug_refuses_a_slug_that_could_escape_the_path() {
        // The slug is spliced into the detail URL, so anything that could
        // leave /anime/{slug} on our own origin must be refused outright.
        assert_eq!(split_slug("https://anidb.app/anime/..%2f..%2fetc-1"), None);
        assert_eq!(split_slug("https://anidb.app/anime/a.b-1"), None);
        // Only the last path segment survives, so traversal in the card href
        // cannot travel with it into the rebuilt url.
        assert_eq!(
            split_slug("https://evil.test/anime/../../x-1"),
            Some(("x-1".into(), "1".into()))
        );
        // No trailing numeric id, or nothing before it.
        assert_eq!(split_slug("https://anidb.app/anime/no-id-here"), None);
        assert_eq!(split_slug("https://anidb.app/anime/-12"), None);
        // Not a show link at all.
        assert_eq!(split_slug("https://anidb.app/about-1"), None);
    }

    #[test]
    fn decode_entities_handles_named_numeric_and_unknown() {
        assert_eq!(decode_entities("Journey&#039;s End"), "Journey's End");
        assert_eq!(decode_entities("Fate&#x2f;Zero"), "Fate/Zero");
        assert_eq!(decode_entities("A &amp; B &lt;c&gt;"), "A & B <c>");
        assert_eq!(decode_entities("100&percnt; sure"), "100&percnt; sure");
        assert_eq!(decode_entities("bare & loose"), "bare & loose");
        assert_eq!(decode_entities("plain"), "plain");
    }

    #[test]
    fn decode_entities_is_single_pass() {
        // The alt attribute is double-escaped; one pass must not over-decode a
        // literal that only looks like an entity.
        assert_eq!(decode_entities("Journey&amp;#039;s"), "Journey&#039;s");
    }

    #[test]
    fn trailing_year_from_the_meta_line() {
        assert_eq!(trailing_year("TV · 2026"), Some(2026));
        assert_eq!(trailing_year("Movie · 1998"), Some(1998));
        assert_eq!(trailing_year("ONA"), None);
        assert_eq!(trailing_year("TV · 12 eps"), None);
    }

    #[test]
    fn parse_external_ids_reads_both_links() {
        let html = r#"<a href="https://myanimelist.net/anime/52991/Sousou">MAL</a>
                      <a href="https://anilist.co/anime/154587" rel="noopener">AniList</a>"#;
        assert_eq!(parse_external_ids(html), (Some(154587), Some(52991)));
    }

    #[test]
    fn parse_external_ids_tolerates_a_missing_side() {
        let html = r#"<a href="https://anilist.co/anime/999">AniList</a>"#;
        assert_eq!(parse_external_ids(html), (Some(999), None));
        assert_eq!(parse_external_ids("<html>nothing</html>"), (None, None));
    }

    #[test]
    fn is_challenge_ignores_the_script_a_good_page_ships() {
        // Live embed pages carry this on a 200. Treating it as a challenge
        // would fail every playback, so it must not be a marker.
        let good = r#"<script src='/cdn-cgi/challenge-platform/scripts/jsd/main.js'></script>
                      <script>var setup = { sources: [{ file: 'https://h/m.m3u8' }] };</script>"#;
        assert!(!is_challenge(good));

        assert!(is_challenge("<title>Just a moment...</title>"));
        assert!(is_challenge("window._cf_chl_opt={cvId:'3'}"));
        assert!(is_challenge(
            "/cdn-cgi/challenge-platform/h/b/jsd/__cf_chl_f_tk"
        ));
    }

    #[test]
    fn is_challenge_ignores_loading_copy_outside_the_title() {
        // "Just a moment" is ordinary body copy. Matching it anywhere would let
        // a synopsis or an app shell take the whole provider offline.
        assert!(!is_challenge(
            r#"<title>Frieren</title><div id="app">Just a moment, loading...</div>"#
        ));
        assert!(!is_challenge("<p>Just a moment of silence.</p>"));
    }

    #[test]
    fn stream_url_ok_refuses_what_must_never_become_a_play_url() {
        assert!(stream_url_ok("https://hls.test/stream/master.m3u8"));
        // Private/loopback: under a quality cap we would fetch this ourselves.
        assert!(!stream_url_ok(
            "http://169.254.169.254/latest/meta-data/x.m3u8"
        ));
        assert!(!stream_url_ok("http://127.0.0.1:8080/x.m3u8"));
        assert!(!stream_url_ok("http://localhost/x.m3u8"));
        assert!(!stream_url_ok("http://10.0.0.5/x.m3u8"));
        // Non-http, relative, and argv-hostile.
        assert!(!stream_url_ok("file:///etc/passwd"));
        assert!(!stream_url_ok("/relative/master.m3u8"));
        assert!(!stream_url_ok("https://h.test/a b.m3u8"));
        assert!(!stream_url_ok("https://h.test/x.m3u8\r\nX-Evil: 1"));
        assert!(!stream_url_ok(""));
    }

    #[test]
    fn extract_hls_prefers_the_player_source_over_an_earlier_m3u8() {
        // Anything on the page can carry a .m3u8 ahead of the real source; the
        // configured `file` is the one the player would actually load.
        let html = r#"<img data-preview="https://ads.test/decoy.m3u8">
            <script>var setup = { sources: [{ file: 'https://hls.test/real/master.m3u8' }] };</script>"#;
        assert_eq!(
            extract_hls(html).as_deref(),
            Some("https://hls.test/real/master.m3u8")
        );
    }

    #[test]
    fn extract_hls_falls_back_when_the_config_key_moves() {
        // No `file` key: the scan still finds a stream, which is the point of
        // keeping the fallback.
        let html = r#"var s = { src: "https://hls.test/only/master.m3u8" };"#;
        assert_eq!(
            extract_hls(html).as_deref(),
            Some("https://hls.test/only/master.m3u8")
        );
        // A `file` that is not a stream must not shadow a real one.
        let html = r#"{"file": "poster.jpg", "src": "https://hls.test/x.m3u8"}"#;
        assert_eq!(
            extract_hls(html).as_deref(),
            Some("https://hls.test/x.m3u8")
        );
    }

    #[test]
    fn extract_hls_from_the_jwplayer_setup() {
        let html = r#"var setup = {
            sources: [{ file: 'https://hls.test/stream/abc/master.m3u8', type: 'hls' }],
            width: '100%',
        };"#;
        assert_eq!(
            extract_hls(html).as_deref(),
            Some("https://hls.test/stream/abc/master.m3u8")
        );
    }

    #[test]
    fn extract_hls_handles_double_quotes_and_refuses_junk() {
        assert_eq!(
            extract_hls(r#"{"file":"https://h/x.m3u8"}"#).as_deref(),
            Some("https://h/x.m3u8")
        );
        assert_eq!(extract_hls("<html>no player</html>"), None);
        // Relative: this url reaches mpv, so a non-absolute one is not usable.
        assert_eq!(extract_hls("file: '/rel/master.m3u8'"), None);
        // Unquoted: nothing to bound the url with.
        assert_eq!(extract_hls("see master.m3u8 somewhere"), None);
    }

    const EPISODES: &[u8] = br#"{"episodes":[
        {"id":3064,"number":3,"filler":false},
        {"id":3062,"number":1,"filler":false},
        {"id":3063,"number":2,"filler":true}
    ]}"#;

    #[test]
    fn parse_episodes_sorts_and_filters() {
        let eps = parse_episodes(EPISODES).unwrap();
        assert_eq!(eps.iter().map(|e| e.number).collect::<Vec<_>>(), [1, 2, 3]);
        assert_eq!(eps[0].id, 3062);

        let junk = br#"{"episodes":[
            {"id":0,"number":1},{"id":5,"number":0},{"id":6,"number":null},
            {"number":9},{"id":7,"number":-2},{"id":8,"number":4}
        ]}"#;
        let eps = parse_episodes(junk).unwrap();
        assert_eq!(eps.len(), 1);
        assert_eq!(eps[0], Episode { id: 8, number: 4 });
    }

    #[test]
    fn parse_episodes_dedupes_repeated_numbers_keeping_the_first_listed() {
        // Which duplicate survives must be the site's first, not whichever the
        // sort happened to leave in front, so the id we serve is deterministic.
        let dupes = br#"{"episodes":[
            {"id":11,"number":2},{"id":12,"number":2},{"id":13,"number":2},
            {"id":21,"number":1},{"id":22,"number":1}
        ]}"#;
        let eps = parse_episodes(dupes).unwrap();
        assert_eq!(eps.len(), 2);
        assert_eq!(eps[0], Episode { id: 21, number: 1 });
        assert_eq!(eps[1], Episode { id: 11, number: 2 });
    }

    #[test]
    fn parse_episodes_empty_and_malformed() {
        assert!(parse_episodes(br#"{"episodes":[]}"#).unwrap().is_empty());
        assert!(parse_episodes(b"{}").unwrap().is_empty());
        assert!(matches!(
            parse_episodes(b"<html>404</html>"),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn base_offset_normalizes_absolute_season_numbering() {
        let one_based = [Episode { id: 1, number: 1 }, Episode { id: 2, number: 2 }];
        assert_eq!(base_offset(&one_based), 0);
        assert_eq!(label(&one_based[1], 0), "2");

        // Frieren S2: the site numbers 29..38, its AniList entry numbers 1..10.
        let absolute = [
            Episode {
                id: 26020,
                number: 29,
            },
            Episode {
                id: 26021,
                number: 30,
            },
        ];
        let offset = base_offset(&absolute);
        assert_eq!(offset, 28);
        assert_eq!(label(&absolute[0], offset), "1");
        assert_eq!(label(&absolute[1], offset), "2");

        assert_eq!(base_offset(&[]), 0);
    }

    // -- seam ---------------------------------------------------------------

    #[test]
    fn canonical_key_is_none_because_the_site_has_no_canonical_endpoint() {
        let p = AniDbApp::new().unwrap();
        let show = Enrichment {
            anilist_id: 154587,
            mal_id: Some(52991),
            ..Enrichment::default()
        };
        assert_eq!(p.canonical_key(&show), None);
    }

    #[test]
    fn supports_search_because_nothing_else_can_bind_it() {
        // canonical_key never answers, so search off would make the provider
        // permanently unbindable.
        let p = AniDbApp::new().unwrap();
        assert!(p.supports_search());
        assert!(p.canonical_key(&Enrichment::default()).is_none());
    }

    #[test]
    fn episodes_rejects_a_non_numeric_show_id() {
        let p = AniDbApp::new().unwrap();
        assert!(matches!(
            p.episodes("../7", Translation::Sub, None),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn resolve_rejects_bad_episode_labels() {
        let p = AniDbApp::new().unwrap();
        for bad in ["0", "abc", "", "-1", "1.5"] {
            assert!(
                matches!(
                    p.resolve("1663", bad, Translation::Sub, Quality::Best),
                    Err(ProviderError::Decode(_))
                ),
                "label {bad:?} must not reach the wire"
            );
        }
    }

    #[test]
    fn cover_request_takes_absolute_refs_only() {
        let p = AniDbApp::new().unwrap();
        let got = p.cover_request("https://cdn.test/poster.jpg").unwrap();
        assert_eq!(got.url, "https://cdn.test/poster.jpg");
        assert_eq!(got.referer.as_deref(), Some(REFERER));

        for bad in ["", "/poster.jpg", "https://cdn.test/a b.jpg"] {
            assert!(p.cover_request(bad).is_err(), "ref {bad:?} must be refused");
        }
        assert!(p.cover_request(&"x".repeat(MAX_COVER_REF_LEN + 1)).is_err());
    }

    // -- transport ----------------------------------------------------------

    /// Path-routed test server. Answers each request from `routes` (404
    /// otherwise) and records the paths it was asked for.
    fn serve_routes(routes: Vec<(String, Vec<u8>)>) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        std::thread::spawn(move || {
            while let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 8192];
                let n = std::io::Read::read(&mut sock, &mut buf).unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                let body = routes
                    .iter()
                    .find(|(p, _)| *p == path)
                    .map(|(_, r)| r.clone())
                    .unwrap_or_else(|| response_with_body("404 Not Found", b"nope"));
                log.lock().unwrap().push(path);
                let _ = std::io::Write::write_all(&mut sock, &body);
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn ok(body: &[u8]) -> Vec<u8> {
        response_with_body("200 OK", body)
    }

    /// Embeds point at a private address on purpose: the SSRF guard then ends
    /// every resolve chain deterministically, instead of a test reaching the
    /// live site.
    fn langs(codes: &[&str]) -> Vec<u8> {
        let rows: Vec<String> = codes
            .iter()
            .map(|c| format!(r#"{{"code":"{c}","embed_url":"http://169.254.169.254/embed/{c}"}}"#))
            .collect();
        ok(format!(r#"{{"languages":[{}]}}"#, rows.join(",")).as_bytes())
    }

    /// A show of `n` episodes numbered from `first`, dubbed through the first
    /// `dubbed` of them. Episode ids are 1000 + index.
    fn show_routes(site: &str, first: u32, n: u32, dubbed: u32) -> Vec<(String, Vec<u8>)> {
        let rows: Vec<String> = (0..n)
            .map(|i| format!(r#"{{"id":{},"number":{}}}"#, 1000 + i, first + i))
            .collect();
        let mut routes = vec![(
            format!("/api/frontend/anime/{site}/episodes"),
            ok(format!(r#"{{"episodes":[{}]}}"#, rows.join(",")).as_bytes()),
        )];
        for i in 0..n {
            let codes: &[&str] = if i < dubbed {
                &["eng", "jpn"]
            } else {
                &["jpn"]
            };
            routes.push((
                format!("/api/frontend/episode/{}/languages", 1000 + i),
                langs(codes),
            ));
        }
        routes
    }

    fn against(routes: Vec<(String, Vec<u8>)>) -> (AniDbApp, Arc<Mutex<Vec<String>>>) {
        let (url, seen) = serve_routes(routes);
        (AniDbApp::with_endpoint(url).unwrap(), seen)
    }

    #[test]
    fn transport_sub_lists_every_episode_without_probing_languages() {
        let (p, seen) = against(show_routes("1663", 1, 5, 0));
        let eps = p.episodes("1663", Translation::Sub, None).unwrap();
        assert_eq!(eps, ["1", "2", "3", "4", "5"]);
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.len(),
            1,
            "sub must not spend a language call: {seen:?}"
        );
    }

    #[test]
    fn transport_sub_labels_are_offset_to_the_canonical_numbering() {
        let (p, _) = against(show_routes("1665", 29, 10, 0));
        let eps = p.episodes("1665", Translation::Sub, None).unwrap();
        assert_eq!(eps.first().map(String::as_str), Some("1"));
        assert_eq!(eps.last().map(String::as_str), Some("10"));
    }

    #[test]
    fn transport_dub_absent_is_not_stocked_for_the_track() {
        let (p, seen) = against(show_routes("1663", 1, 12, 0));
        let eps = p.episodes("1663", Translation::Dub, None).unwrap();
        assert!(eps.is_empty(), "no dub must report not-stocked");
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "one episodes call, then episode 1 settles it"
        );
    }

    #[test]
    fn transport_dub_fully_dubbed_takes_two_probes() {
        let (p, seen) = against(show_routes("1663", 1, 28, 28));
        let eps = p.episodes("1663", Translation::Dub, None).unwrap();
        assert_eq!(eps.len(), 28);
        assert_eq!(eps.last().map(String::as_str), Some("28"));
        assert_eq!(
            seen.lock().unwrap().len(),
            3,
            "first and last dubbed ends the search"
        );
    }

    #[test]
    fn transport_dub_bisects_the_boundary() {
        // 64 episodes, dubbed through 40. Linear probing would cost 40+ calls.
        let (p, seen) = against(show_routes("3880", 1, 64, 40));
        let eps = p.episodes("3880", Translation::Dub, None).unwrap();
        assert_eq!(eps.len(), 40);
        assert_eq!(eps.last().map(String::as_str), Some("40"));

        let calls = seen.lock().unwrap().len();
        assert!(
            calls <= 10,
            "bisect must stay logarithmic, took {calls} calls"
        );
    }

    #[test]
    fn transport_dub_boundary_holds_under_the_offset() {
        let (p, _) = against(show_routes("1665", 29, 10, 4));
        let eps = p.episodes("1665", Translation::Dub, None).unwrap();
        assert_eq!(eps, ["1", "2", "3", "4"]);
    }

    #[test]
    fn transport_dub_single_episode_show() {
        let (p, _) = against(show_routes("77", 1, 1, 1));
        assert_eq!(p.episodes("77", Translation::Dub, None).unwrap(), ["1"]);
        let (p, _) = against(show_routes("78", 1, 1, 0));
        assert!(p.episodes("78", Translation::Dub, None).unwrap().is_empty());
    }

    #[test]
    fn transport_episodes_missing_show_is_an_error_not_absence() {
        // A 404 must not read as "not stocked", or it stamps a 7-day absence.
        let (p, _) = against(Vec::new());
        assert!(matches!(
            p.episodes("1663", Translation::Sub, None),
            Err(ProviderError::Http { status: 404 })
        ));
    }

    #[test]
    fn transport_resolve_maps_the_label_through_the_offset() {
        // Label 1 on a 29-based show must ask for episode 29's languages. The
        // embed itself is loopback here, so the SSRF guard stops the chain
        // right after; the requested path is the proof.
        let (p, seen) = against(show_routes("1665", 29, 10, 0));
        let got = p.resolve("1665", "3", Translation::Sub, Quality::Best);
        assert!(matches!(got, Err(ProviderError::Decode(_))));
        let seen = seen.lock().unwrap();
        assert_eq!(
            seen.last().map(String::as_str),
            // Third episode of the show: id 1002, site number 31.
            Some("/api/frontend/episode/1002/languages")
        );
    }

    #[test]
    fn transport_resolve_refuses_a_private_embed_url() {
        // The embed url is provider-supplied and gets fetched, so it must clear
        // the SSRF guard before anything dials it.
        let (p, _) = against(show_routes("1663", 1, 1, 0));
        let got = p.resolve("1663", "1", Translation::Sub, Quality::Best);
        assert!(
            matches!(&got, Err(ProviderError::Decode(msg)) if msg == "blocked embed url"),
            "expected the guard to stop it, got {got:?}"
        );
    }

    #[test]
    fn transport_resolve_wrong_track_is_a_clean_miss() {
        let (p, _) = against(show_routes("1663", 1, 3, 0));
        assert!(matches!(
            p.resolve("1663", "1", Translation::Dub, Quality::Best),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn transport_resolve_unknown_episode_is_a_clean_miss() {
        let (p, _) = against(show_routes("1663", 1, 3, 0));
        assert!(matches!(
            p.resolve("1663", "99", Translation::Sub, Quality::Best),
            Err(ProviderError::Decode(_))
        ));
    }

    fn opts() -> SearchOptions {
        SearchOptions {
            translation: Translation::Sub,
            limit: 26,
            page: 1,
        }
    }

    #[test]
    fn transport_search_probes_each_card_for_its_ids() {
        let detail = |anilist: i64, mal: i64| {
            ok(format!(
                r#"<a href="https://myanimelist.net/anime/{mal}/x">MAL</a>
                   <a href="https://anilist.co/anime/{anilist}">AniList</a>"#
            )
            .as_bytes())
        };
        let (p, seen) = against(vec![
            ("/search/suggestions?q=frieren".into(), ok(CARDS.as_bytes())),
            (
                "/anime/frieren-beyond-journeys-end-season-2-1665".into(),
                detail(182255, 59978),
            ),
            (
                "/anime/frieren-beyond-journeys-end-1663".into(),
                detail(154587, 52991),
            ),
        ]);

        let hits = p.search("frieren", &opts()).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].provider_id, "1665");
        assert_eq!(hits[0].anilist_id, Some(182255));
        assert_eq!(hits[0].mal_id, Some(59978));
        assert_eq!(hits[0].year, Some(2026));
        assert_eq!(hits[1].provider_id, "1663");
        assert_eq!(hits[1].anilist_id, Some(154587));
        assert_eq!(seen.lock().unwrap().len(), 3);

        // The ids are the whole point: they let the scorer separate the two
        // seasons that a title match cannot.
        let s1 = Enrichment {
            anilist_id: 154587,
            mal_id: Some(52991),
            ..Enrichment::default()
        };
        assert_eq!(crate::resolver::best_id_match(&s1, &hits), Some(1));
    }

    #[test]
    fn transport_search_survives_a_detail_page_that_fails() {
        let (p, _) = against(vec![(
            "/search/suggestions?q=frieren".into(),
            ok(CARDS.as_bytes()),
        )]);
        // Both detail probes 404; the cards still ship for title matching.
        let hits = p.search("frieren", &opts()).unwrap();
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.anilist_id.is_none()));
        assert_eq!(hits[0].title, "Frieren: Beyond Journey's End Season 2");
    }

    #[test]
    fn transport_search_percent_encodes_the_query() {
        let (p, seen) = against(vec![(
            "/search/suggestions?q=fate%2Fzero+%26+co".into(),
            ok(b""),
        )]);
        assert!(p.search("fate/zero & co", &opts()).unwrap().is_empty());
        assert_eq!(
            seen.lock().unwrap().first().map(String::as_str),
            Some("/search/suggestions?q=fate%2Fzero+%26+co"),
            "query metacharacters must stay inside the q param"
        );
    }

    #[test]
    fn transport_search_challenge_page_is_a_block_not_an_empty_result() {
        // Reading a challenge as zero results would stamp a 7-day absence.
        let (p, _) = against(vec![(
            "/search/suggestions?q=frieren".into(),
            ok(b"<title>Just a moment...</title>"),
        )]);
        assert!(matches!(
            p.search("frieren", &opts()),
            Err(ProviderError::Forbidden { status: 403 })
        ));
    }

    #[test]
    fn transport_search_challenge_on_a_detail_page_stops_the_walk() {
        let (p, seen) = against(vec![
            ("/search/suggestions?q=frieren".into(), ok(CARDS.as_bytes())),
            (
                "/anime/frieren-beyond-journeys-end-season-2-1665".into(),
                ok(b"window._cf_chl_opt={}"),
            ),
        ]);
        assert!(matches!(
            p.search("frieren", &opts()),
            Err(ProviderError::Forbidden { status: 403 })
        ));
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "a block must stop the probe loop, not run all 8"
        );
    }

    #[test]
    fn search_short_query_and_later_pages_answer_offline() {
        let p = AniDbApp::with_endpoint("http://127.0.0.1:1".to_string()).unwrap();
        assert!(p.search("a", &opts()).unwrap().is_empty());
        assert!(p.search("", &opts()).unwrap().is_empty());
        let page2 = SearchOptions { page: 2, ..opts() };
        assert!(p.search("frieren", &page2).unwrap().is_empty());
    }

    #[test]
    fn transport_search_truncates_to_the_requested_limit() {
        let (p, _) = against(vec![(
            "/search/suggestions?q=frieren".into(),
            ok(CARDS.as_bytes()),
        )]);
        let one = SearchOptions { limit: 1, ..opts() };
        assert_eq!(p.search("frieren", &one).unwrap().len(), 1);
    }
}
