//! AllAnime `StreamProvider` (03 §8.3): registry backstop, first concrete
//! provider in the port (ROD-436).
//!
//! Protocol (reimplemented from anipy-cli traffic, GPL-3.0, no code copied;
//! ROD-91 / ROD-62 / ROD-55): POST not GET (Cloudflare only challenges GET);
//! Apollo persisted-query sha256 hashes; AES-256-GCM `tobeparsed` blob.
//!
//! Site-specific facts (endpoint, hashes, referers, decrypt) stay quarantined
//! here behind `StreamProvider`. When AllAnime dies: replace this file.

use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::Duration;

use super::hls;
use super::http::{Accept, HttpClient, Method, Request};
use crate::domain::{Enrichment, Quality, StreamLink, Translation};
use crate::fetchguard::guard_fetch_url;
use crate::providers::{CoverRequest, ProviderError, SEARCH_PAGE_SIZE, SearchHit, StreamProvider};

const API: &str = "https://api.allanime.day/api";
// Old Chrome UA: accepted and unremarkable.
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; WOW64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/86.0.4240.198 Safari/537.36";

// Apollo persisted-query hashes (server identifies ops by these, not raw
// query).
const HASH_SEARCH: &str = "a24c500a1b765c68ae1d8dd85174931f661c71369c89b92b88b75a725afc471c";
const HASH_EPISODES: &str = "043448386c7a686bc2aabfbb6b80f6074e795d350df48015023b079527b0848a";
const HASH_VIDEO: &str = "d405d0edd690624b66baba3068e0edc3ac90f1597d898a1ec8db4e5c43c00fec";

// AES-256-GCM key seed for `tobeparsed` (key = sha256(seed)).
const GCM_SEED: &[u8] = b"Xot36i3lK3:v1";

// Site origin: deciphered provider GETs (ROD-92) and CDN referer.
const SITE: &str = "https://allanime.day";

// Cover CDN for bare relative `mcovers/…` paths (absolute AniList/MAL urls
// pass through). Cloudflare-fronted; 403s without referer (ROD-267).
const COVER_CDN_BASE: &str = "https://wp.youtube-anime.com/aln.youtube-anime.com/";

// Cap before splicing a cover ref into a fetch URL (ROD-267).
const MAX_COVER_REF_LEN: usize = 2048;

// Referers the API / CDN gate on.
const REFERER_API: &str = "https://allmanga.to/"; // search + episodes + clock GET
const REFERER_VIDEO: &str = "https://youtu-chan.com/"; // get_video
const STREAM_REFERER: &str = SITE; // mpv → fast4speed CDN

/// Long-tail GET wall-clock cap (ROD-153). KB payloads; trips only on stall.
/// Without it, one slow host freezes resolve's sequential loop.
const FETCH_DEADLINE: Duration = Duration::from_secs(20);

// anipy trusted sourceName allow-list (fast4speed + long-tail).
const ALLOWED_SOURCES: [&str; 5] = ["Yt-mp4", "S-Mp4", "Uv-mp4", "Ak", "Default"];

/// `extensions` value: a JSON *string* of JSON; serde escapes the inner
/// quotes when the body serializes.
fn ext_json(hash: &str) -> String {
    format!("{{\"persistedQuery\":{{\"version\":1,\"sha256Hash\":\"{hash}\"}}}}")
}

/// Search `variables` is a plain object (not stringified; per-op quirk). One
/// page of `SEARCH_PAGE_SIZE` so stride matches load-more (ROD-201).
fn search_body(query: &str, page: u32, translation: Translation) -> String {
    json!({
        "variables": {
            "search": { "query": query },
            "limit": SEARCH_PAGE_SIZE,
            "page": page,
            "translationType": translation.as_str(),
            "countryOrigin": "ALL",
        },
        "extensions": ext_json(HASH_SEARCH),
    })
    .to_string()
}

/// Episodes/video `variables` is a stringified JSON object (per-op quirk,
/// opposite of search); `json!` escapes the id, the outer layer escapes the
/// inner string.
fn episodes_body(show_id: &str) -> String {
    let inner = json!({ "_id": show_id }).to_string();
    json!({ "variables": inner, "extensions": ext_json(HASH_EPISODES) }).to_string()
}

fn video_body(show_id: &str, translation: Translation, episode: &str) -> String {
    let inner = json!({
        "showId": show_id,
        "translationType": translation.as_str(),
        "episodeString": episode,
    })
    .to_string();
    json!({ "variables": inner, "extensions": ext_json(HASH_VIDEO) }).to_string()
}

// ── search DTOs ─────────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct AvailEps {
    #[serde(default)]
    sub: u32,
    #[serde(default)]
    dub: u32,
}

#[derive(Deserialize)]
struct SEdge {
    #[serde(rename = "_id")]
    id: String,
    name: Option<String>,
    #[serde(rename = "englishName")]
    english_name: Option<String>,
    #[serde(rename = "nativeName")]
    native_name: Option<String>,
    thumbnail: Option<String>,
    #[serde(rename = "availableEpisodes", default)]
    available_episodes: AvailEps,
    #[serde(rename = "airedStart")]
    aired_start: Option<AiredStart>,
    season: Option<SeasonObj>,
}

#[derive(Deserialize, Default)]
struct AiredStart {
    year: Option<u32>,
}

#[derive(Deserialize, Default)]
struct SeasonObj {
    year: Option<u32>,
}

#[derive(Deserialize)]
struct SShows {
    edges: Vec<SEdge>,
}

#[derive(Deserialize)]
struct SData {
    shows: SShows,
}

#[derive(Deserialize)]
struct SResp {
    data: Option<SData>,
}

/// Mine the AniList media id from a cover filename
/// `…/anilistcdn/media/anime/cover/…/bx182255-hash.jpg` (ROD-181). Leading
/// letters are size/kind; digits are the id. None for the MAL CDN (~13%) and
/// unknown shapes (caller falls back to title match).
///
/// TRUST: assumes the thumb truthfully names the show. The mined id is only a
/// tier-B claim; `best_id_match`'s contradiction veto (eps/year) catches an
/// ACCIDENTAL mis-stamp, but a fully hostile source that forges a thumb naming
/// the victim id AND matching eps/year clears the veto (no title floor on tier
/// B by design, ROD-342). That is the provider trust model, freeze-parity: a
/// compromised allanime can already mis-serve streams. Documented, not closed.
fn anilist_id_from_thumb(url: Option<&str>) -> Option<i64> {
    let url = url?;
    if !url.contains("anilistcdn/media/anime/cover/") {
        return None;
    }
    let file = &url[url.rfind('/')? + 1..];
    let digits_at = file.find(|c: char| !c.is_ascii_alphabetic())?;
    let rest = &file[digits_at..];
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..digits_end].parse().ok()
}

/// Search edge → tier-C candidate. Year: airedStart, else season year
/// (ROD-181). Score/season/format chips have no `SearchHit` home: provider
/// search feeds binding only, never Browse (03 §1).
fn edge_to_hit(e: SEdge) -> SearchHit {
    let aired_year = e.aired_start.and_then(|a| a.year);
    let season_year = e.season.and_then(|s| s.year);
    SearchHit {
        provider_id: e.id,
        anilist_id: anilist_id_from_thumb(e.thumbnail.as_deref()),
        title: e.name.unwrap_or_else(|| "(untitled)".to_string()),
        title_english: e.english_name,
        title_native: e.native_name,
        mal_id: None,
        total_episodes: None,
        eps_sub: e.available_episodes.sub,
        eps_dub: e.available_episodes.dub,
        year: aired_year.or(season_year),
    }
}

/// Title-match bonus + log2 episode-count tiebreak (fuller series wins,
/// ROD-60); AllAnime's popularity score is metadata only.
fn relevance(name: &str, query: &str, eps: u32) -> f64 {
    let n = name.to_ascii_lowercase();
    let q = query.to_ascii_lowercase();
    let base = if n == q {
        1000.0
    } else if n.starts_with(&q) {
        500.0
    } else if n.contains(&q) {
        250.0
    } else {
        0.0
    };
    base + (f64::from(eps) + 2.0).log2()
}

// ── episodes DTOs ───────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct EpDetail {
    #[serde(default)]
    sub: Vec<String>,
    #[serde(default)]
    dub: Vec<String>,
}

#[derive(Deserialize)]
struct EShow {
    #[serde(rename = "availableEpisodesDetail", default)]
    available_episodes_detail: EpDetail,
}

#[derive(Deserialize)]
struct EData {
    show: Option<EShow>,
}

#[derive(Deserialize)]
struct EResp {
    data: Option<EData>,
}

// ── resolve DTOs ────────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct VData {
    tobeparsed: Option<String>,
}

#[derive(Deserialize)]
struct VResp {
    data: Option<VData>,
}

#[derive(Deserialize)]
struct Src {
    #[serde(rename = "sourceName")]
    source_name: Option<String>,
    #[serde(rename = "sourceUrl")]
    source_url: Option<String>,
}

#[derive(Deserialize)]
struct DecEp {
    #[serde(rename = "sourceUrls")]
    source_urls: Vec<Src>,
}

#[derive(Deserialize)]
struct Dec {
    episode: DecEp,
}

// Case-insensitive (ROD-178): API sends `S-mp4`, list has `S-Mp4`.
fn source_allowed(name: Option<&str>) -> bool {
    name.is_some_and(|n| ALLOWED_SOURCES.iter().any(|a| a.eq_ignore_ascii_case(n)))
}

/// base64 + AES-256-GCM `tobeparsed`. Layout: [0] prefix, [1..13] nonce,
/// [13..] ciphertext||tag. Key = sha256(GCM_SEED). Golden vector pinned by
/// spike_stream and the test below; GCM fails closed on any corruption.
fn decrypt_tobeparsed(blob: &str) -> Result<Vec<u8>, ProviderError> {
    let key = Sha256::digest(GCM_SEED);
    let cipher = Aes256Gcm::new_from_slice(key.as_slice())
        .map_err(|_| ProviderError::Decode("bad GCM key length".into()))?;
    // Indifferent padding: the blob is unpadded standard base64.
    let engine = GeneralPurpose::new(
        &base64::alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let raw = engine
        .decode(blob)
        .map_err(|_| ProviderError::Decode("tobeparsed is not base64".into()))?;
    if raw.len() < 1 + 12 + 16 {
        return Err(ProviderError::Decode("tobeparsed blob too small".into()));
    }
    cipher
        .decrypt(Nonce::from_slice(&raw[1..13]), &raw[13..])
        .map_err(|_| ProviderError::Decode("GCM authentication failed".into()))
}

/// `--<hex>` provider path: hex pairs XOR 0x38. anipy's oct()/int(_,8) wrap
/// is a no-op (verified all 256 values); dropped. Caller strips `--`, then
/// `clock_json`.
fn decipher_provider_path(hex: &str) -> Result<String, ProviderError> {
    if !hex.len().is_multiple_of(2) || !hex.is_ascii() {
        return Err(ProviderError::Decode("bad provider path".into()));
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).map(|b| b ^ 0x38))
        .collect::<Result<Vec<u8>, _>>()
        .map_err(|_| ProviderError::Decode("bad provider path".into()))?;
    String::from_utf8(bytes).map_err(|_| ProviderError::Decode("bad provider path".into()))
}

/// First `clock` → `clock.json` (oracle .replace; paths are
/// `/apivtwo/clock?…`).
fn clock_json(path: &str) -> String {
    match path.find("clock") {
        Some(at) => {
            let cut = at + "clock".len();
            format!("{}.json{}", &path[..cut], &path[cut..])
        }
        None => path.to_string(),
    }
}

/// Wixmp repackager `…,480p,720p,…<tail>.urlset/…` → per-quality URLs. None
/// if not a wixmp repackager link.
fn wixmp_variants(link: &str) -> Result<Option<Vec<hls::Variant>>, ProviderError> {
    if !link.contains("repackager.wixmp.com") {
        return Ok(None);
    }
    let head = &link[..link.find(".urlset").unwrap_or(link.len())];
    // Global host strip (oracle .replace); first/last parts wrap each quality.
    let body = head.replace("repackager.wixmp.com/", "");
    let parts: Vec<&str> = body.split(',').collect();
    if parts.len() < 3 {
        return Err(ProviderError::Decode("bad wixmp url".into())); // wrap + ≥1 quality + wrap
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    let mut out = Vec::new();
    for qual in &parts[1..parts.len() - 1] {
        let digits = qual.strip_suffix('p').unwrap_or(qual);
        out.push(hls::Variant {
            url: format!("{first}{qual}{last}"),
            resolution: digits.parse().ok(),
        });
    }
    Ok(Some(out))
}

/// Safe for mpv argv: printable ASCII 0x21-0x7e only (allowlist, not
/// denylist). Catches CR/LF and ≥0x80 line-break-equivalents a `<0x20`
/// denylist would miss.
fn clean_arg(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|c| (0x21..=0x7e).contains(&c))
}

/// Untrusted clock.json Referer → SITE if dirty/absent (header-injection
/// hazard).
fn safe_referer(r: Option<&str>) -> &str {
    match r {
        Some(v) if clean_arg(v) => v,
        _ => SITE,
    }
}

/// Candidate → StreamLink or None. Must be http(s) (also rejects a leading
/// `--` mpv would treat as an option) and clean. Quality pick is
/// `select_variant`'s job.
fn consider(url: &str, resolution: Option<u32>, referer: &str) -> Option<StreamLink> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return None;
    }
    if !clean_arg(url) {
        return None;
    }
    Some(StreamLink {
        url: url.to_string(),
        resolution,
        referer: Some(referer.to_string()),
        user_agent: None,
        cloaked_segments: false,
        decloak_segments: false,
        sub_url: None,
    })
}

/// Direct fast4speed URL through `consider` before mpv (ROD-396 F3). None →
/// long tail. The blob rides public-seed GCM; a MITM could forge a trusted
/// substring + argv injection, so `consider` is the gate here too.
fn fast4speed_pick(sources: &[Src]) -> Option<StreamLink> {
    for s in sources {
        if !source_allowed(s.source_name.as_deref()) {
            continue;
        }
        let Some(url) = s.source_url.as_deref() else {
            continue;
        };
        if !url.contains("tools.fast4speed.rsvp") {
            continue;
        }
        if let Some(sl) = consider(url, Some(1080), STREAM_REFERER) {
            return Some(sl);
        }
    }
    None
}

// ── clock.json DTOs ─────────────────────────────────────────────────────────

#[derive(Deserialize, Default)]
struct ClkHdr {
    #[serde(rename = "Referer")]
    referer: Option<String>,
}

#[derive(Deserialize)]
struct ClkLink {
    link: Option<String>,
    headers: Option<ClkHdr>,
}

#[derive(Deserialize, Default)]
struct ClkResp {
    #[serde(default)]
    links: Vec<ClkLink>,
}

pub struct AllAnime {
    http: HttpClient,
    api: String,
    site: String,
}

impl AllAnime {
    pub fn new() -> Result<AllAnime, ProviderError> {
        AllAnime::with_endpoints(API.to_string(), SITE.to_string())
    }

    fn with_endpoints(api: String, site: String) -> Result<AllAnime, ProviderError> {
        Ok(AllAnime {
            http: HttpClient::new()?,
            api,
            site,
        })
    }

    /// GraphQL POST. `OkOnly`: a non-200 2xx is drift. Referer splits
    /// search/episodes vs get_video.
    fn post(&self, body: &str, referer: &str) -> Result<Vec<u8>, ProviderError> {
        self.http.fetch(&Request {
            method: Method::Post,
            url: &self.api,
            payload: Some(("application/json", body.as_bytes())),
            user_agent: UA,
            extra_headers: &[("Referer", referer)],
            accept: Accept::OkOnly,
            deadline: None,
        })
    }

    /// ROD-92 long-tail GET: SSRF-guard, redirects refused (client-wide),
    /// body cap, 20s wall clock.
    fn get(&self, url: &str, referer: &str) -> Result<Vec<u8>, ProviderError> {
        guard_fetch_url(url).map_err(|_| ProviderError::Decode("blocked url".into()))?;
        self.http.fetch(&Request {
            method: Method::Get,
            url,
            payload: None,
            user_agent: UA,
            extra_headers: &[("Referer", referer)],
            accept: Accept::OkOnly,
            deadline: Some(FETCH_DEADLINE),
        })
    }

    /// Follow one `--<hex>` provider; append safe variants. A failed *link*
    /// is skipped; a failed *provider* errors (partial appends kept). Quality
    /// pick is later, once over the full set (ROD-152).
    fn follow_provider(&self, hex: &str, out: &mut Vec<StreamLink>) -> Result<(), ProviderError> {
        let path = clock_json(&decipher_provider_path(hex)?);
        // Path must start with `/` or SITE becomes userinfo (`@evil/x` SSRF).
        if !path.starts_with('/') {
            return Err(ProviderError::Decode("bad provider path".into()));
        }
        let raw = self.get(&format!("{}{}", self.site, path), REFERER_API)?;
        let resp: ClkResp = serde_json::from_slice(&raw)
            .map_err(|e| ProviderError::Decode(format!("clock.json: {e}")))?;

        for l in resp.links {
            let Some(link) = l.link else { continue };
            let referer = safe_referer(l.headers.as_ref().and_then(|h| h.referer.as_deref()));

            // Shape 1: wixmp repackager (synthetic per-quality URLs).
            if let Some(vs) = wixmp_variants(&link)? {
                for v in vs {
                    out.extend(consider(&v.url, v.resolution, STREAM_REFERER));
                }
                continue;
            }

            // Shape 2: m3u8 master (or media playlist if no variants).
            let Ok(body) = self.get(&link, referer) else {
                continue;
            };
            let vs = hls::parse_master_playlist(&String::from_utf8_lossy(&body));
            if vs.is_empty() {
                out.extend(consider(&link, Some(1080), referer));
            } else {
                for v in vs {
                    if let Some(joined) = hls::join_url(&link, &v.url) {
                        out.extend(consider(&joined, v.resolution, referer));
                    }
                }
            }
        }
        Ok(())
    }

    fn parse_search(
        raw: &[u8],
        query: &str,
        translation: Translation,
        limit: u32,
    ) -> Result<Vec<SearchHit>, ProviderError> {
        let resp: SResp = serde_json::from_slice(raw)
            .map_err(|e| ProviderError::Decode(format!("search: {e}")))?;
        // data:null on HTTP 200 = operation rejected (often a rotated hash);
        // transient, never absence.
        let data = resp
            .data
            .ok_or_else(|| ProviderError::Decode("search rejected (data:null)".into()))?;
        let mut hits: Vec<SearchHit> = data.shows.edges.into_iter().map(edge_to_hit).collect();
        let eps = |h: &SearchHit| match translation {
            Translation::Sub => h.eps_sub,
            Translation::Dub => h.eps_dub,
        };
        hits.sort_by(|a, b| {
            relevance(&b.title, query, eps(b)).total_cmp(&relevance(&a.title, query, eps(a)))
        });
        hits.truncate(limit as usize);
        Ok(hits)
    }

    fn parse_episodes(raw: &[u8], translation: Translation) -> Result<Vec<String>, ProviderError> {
        let resp: EResp = serde_json::from_slice(raw)
            .map_err(|e| ProviderError::Decode(format!("episodes: {e}")))?;
        let data = resp
            .data
            .ok_or_else(|| ProviderError::Decode("episodes rejected (data:null)".into()))?;
        // show:null = unknown id; an error, never an authoritative empty.
        let show = data
            .show
            .ok_or_else(|| ProviderError::Decode("show not found".into()))?;
        let mut eps = match translation {
            Translation::Sub => show.available_episodes_detail.sub,
            Translation::Dub => show.available_episodes_detail.dub,
        };
        eps.sort_by(|a, b| crate::domain::episode_label_cmp(a, b));
        Ok(eps)
    }
}

impl StreamProvider for AllAnime {
    /// DB key for bindings/absences/pins. Never rename.
    fn name(&self) -> &'static str {
        "allanime"
    }

    fn display_name(&self) -> &'static str {
        "AllAnime"
    }

    /// Opaque catalog id only (the AniList id is tier B via thumb mining);
    /// no MAL/AL join here.
    fn canonical_key(&self, _show: &Enrichment) -> Option<String> {
        None
    }

    fn search(
        &self,
        query: &str,
        opts: &super::SearchOptions,
    ) -> Result<Vec<SearchHit>, ProviderError> {
        let raw = self.post(
            &search_body(query, opts.page, opts.translation),
            REFERER_API,
        )?;
        AllAnime::parse_search(&raw, query, opts.translation, opts.limit)
    }

    fn episodes(
        &self,
        provider_id: &str,
        translation: Translation,
        _count_hint: Option<u32>,
    ) -> Result<Vec<String>, ProviderError> {
        let raw = self.post(&episodes_body(provider_id), REFERER_API)?;
        AllAnime::parse_episodes(&raw, translation)
    }

    fn resolve(
        &self,
        provider_id: &str,
        episode: &str,
        translation: Translation,
        quality: Quality,
    ) -> Result<StreamLink, ProviderError> {
        let raw = self.post(
            &video_body(provider_id, translation, episode),
            REFERER_VIDEO,
        )?;
        let resp: VResp = serde_json::from_slice(&raw)
            .map_err(|e| ProviderError::Decode(format!("video: {e}")))?;
        let tbp = resp
            .data
            .and_then(|d| d.tobeparsed)
            .ok_or_else(|| ProviderError::Decode("video rejected (data:null)".into()))?;
        let plain = decrypt_tobeparsed(&tbp)?;
        let dec: Dec = serde_json::from_slice(&plain)
            .map_err(|e| ProviderError::Decode(format!("decrypted payload: {e}")))?;
        let sources = dec.episode.source_urls;

        // Fast path: direct fast4speed, single-variant 1080p (ROD-396 F3);
        // the quality pref has nothing to pick. Unsafe match falls to long
        // tail.
        if let Some(sl) = fast4speed_pick(&sources) {
            log::debug!(
                "allanime resolve: fast4speed direct 1080p, quality={quality:?} not applicable"
            );
            return Ok(sl);
        }

        // Long-tail (ROD-92): `--<hex>` providers. One bad provider doesn't
        // sink the rest.
        let mut variants = Vec::new();
        for s in &sources {
            if !source_allowed(s.source_name.as_deref()) {
                continue;
            }
            let Some(hex) = s.source_url.as_deref().and_then(|u| u.strip_prefix("--")) else {
                continue;
            };
            let _ = self.follow_provider(hex, &mut variants);
        }
        // Sources existed but none playable: CDN failure, not hash rotation.
        let n = variants.len();
        let pick = hls::select_variant(&variants, quality)
            .cloned()
            .ok_or_else(|| ProviderError::Decode("no playable variant".into()))?;
        log::debug!(
            "allanime resolve: quality={quality:?} picked {}p from {n} variant(s)",
            pick.resolution
                .map_or_else(|| "?".to_string(), |r| r.to_string())
        );
        Ok(pick)
    }

    /// Cover ref → fetch request (ROD-267). Absolute as-is; relative
    /// `mcovers/…` gets CDN + SITE referer + UA. Untrusted ref: bound +
    /// printable-ASCII, or CR/LF/space would smuggle headers onto the wire.
    fn cover_request(&self, cover_ref: &str) -> Result<CoverRequest, ProviderError> {
        if cover_ref.is_empty() || cover_ref.len() > MAX_COVER_REF_LEN || !clean_arg(cover_ref) {
            return Err(ProviderError::Decode("invalid cover ref".into()));
        }
        if cover_ref.starts_with("http://") || cover_ref.starts_with("https://") {
            return Ok(CoverRequest {
                url: cover_ref.to_string(),
                referer: None,
                user_agent: None,
            });
        }
        Ok(CoverRequest {
            url: format!("{COVER_CDN_BASE}{cover_ref}"),
            referer: Some(SITE.to_string()),
            user_agent: Some(UA.to_string()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::SearchOptions;
    use crate::testutil::{response_with_body, serve_once};

    // ── pure helpers ────────────────────────────────────────────────────────

    #[test]
    fn anilist_id_from_thumb_mines_cover_urls() {
        let f = |s: &str| anilist_id_from_thumb(Some(s));
        assert_eq!(
            f(
                "https://s4.anilist.co/file/anilistcdn/media/anime/cover/large/bx182255-butzrqd4I0aC.jpg"
            ),
            Some(182255)
        );
        assert_eq!(
            f(
                "https://s4.anilist.co/file/anilistcdn/media/anime/cover/medium/b9203-Dvr3qxjibGHK.png"
            ),
            Some(9203)
        );
        assert_eq!(
            f(
                "https://s4.anilist.co/file/anilistcdn/media/anime/cover/large/nx437-w44gw3LYmLba.jpg"
            ),
            Some(437)
        );
        // MAL path is an image id, not an anime id.
        assert_eq!(
            f("https://cdn.myanimelist.net/images/anime/10/11244.jpg"),
            None
        );
        assert_eq!(anilist_id_from_thumb(None), None);
        assert_eq!(
            f("https://s4.anilist.co/file/anilistcdn/media/anime/cover/large/bx-nope.jpg"),
            None
        );
    }

    #[test]
    fn relevance_exact_over_prefix_over_substring_with_eps_tiebreak() {
        let exact = relevance("Frieren", "Frieren", 12);
        let prefix = relevance("Frieren: Beyond Journey's End", "Frieren", 12);
        let sub = relevance("The World of Frieren", "Frieren", 12);
        let none = relevance("Naruto", "Frieren", 12);
        assert!(exact > prefix && prefix > sub && sub > none);
        assert!(relevance("Frieren", "Frieren", 28) > relevance("Frieren", "Frieren", 1));
        assert!(relevance("FRIEREN", "frieren", 1) > 999.0);
    }

    #[test]
    fn source_allowed_is_case_insensitive() {
        assert!(source_allowed(Some("Default")));
        assert!(source_allowed(Some("S-mp4")));
        assert!(source_allowed(Some("UV-MP4")));
        assert!(!source_allowed(Some("Sak")));
        assert!(!source_allowed(Some("S-mp5")));
        assert!(!source_allowed(None));
    }

    #[test]
    fn consider_and_safe_referer_reject_argv_injection() {
        assert_eq!(safe_referer(Some("https://x/\r\nEvil: 1")), SITE);
        assert_eq!(safe_referer(Some("https://ok.test/")), "https://ok.test/");
        assert_eq!(safe_referer(None), SITE);
        assert!(consider("https://x/a\nb", Some(1080), "r").is_none());
        assert!(consider("--script=evil.lua", Some(720), "r").is_none());
        assert!(consider("ftp://x/v.ts", Some(720), "r").is_none());
        let ok = consider("https://cdn.test/v.m3u8", Some(1080), SITE).unwrap();
        assert_eq!(ok.url, "https://cdn.test/v.m3u8");
        assert_eq!(ok.resolution, Some(1080));
        assert_eq!(ok.referer.as_deref(), Some(SITE));
    }

    #[test]
    fn fast4speed_pick_gates_through_consider() {
        let src = |name: Option<&str>, url: Option<&str>| Src {
            source_name: name.map(String::from),
            source_url: url.map(String::from),
        };
        let clean = [src(
            Some("Default"),
            Some("https://tools.fast4speed.rsvp/hls/v.m3u8"),
        )];
        let got = fast4speed_pick(&clean).unwrap();
        assert_eq!(got.url, "https://tools.fast4speed.rsvp/hls/v.m3u8");
        assert_eq!(got.resolution, Some(1080));

        // Trusted substring + argv injection must drop (else raw to mpv).
        let nl = [src(
            Some("Default"),
            Some("https://tools.fast4speed.rsvp/v\n--script=evil.lua"),
        )];
        assert!(fast4speed_pick(&nl).is_none());
        let dashed = [src(Some("Default"), Some("--tools.fast4speed.rsvp/v.m3u8"))];
        assert!(fast4speed_pick(&dashed).is_none());
        let spoofed = [src(
            Some("spoofed"),
            Some("https://tools.fast4speed.rsvp/x.m3u8"),
        )];
        assert!(fast4speed_pick(&spoofed).is_none());
    }

    #[test]
    fn decrypt_tobeparsed_golden_blob_round_trips() {
        // Pins the layout (prefix/nonce/ct/tag) and key = sha256(GCM_SEED);
        // same offline fixture as zigoku and spike_stream.
        let blob = "AAABAgMEBQYHCAkKCw/k3QdUZIc5wIflWKnNrBJlDJDvuoUtnAhztwaZ0MPdc+7QLkxnnkAqseAyPNsmcPKDx4IlVT/nzzS1VVCzmf7KRsutWoKHB/11G9S8i9qBiKecETa/9Yrge8E1Rv/TJ35g7iREfYhMrh8s";
        let want = r#"{"episode":{"sourceUrls":[{"sourceName":"Default","sourceUrl":"tools.fast4speed.rsvp/x"}]}}"#;
        assert_eq!(decrypt_tobeparsed(blob).unwrap(), want.as_bytes());

        assert!(decrypt_tobeparsed("AAAA").is_err()); // too small
        assert!(decrypt_tobeparsed("not base64 !!").is_err());
    }

    #[test]
    fn decipher_provider_path_golden_vector() {
        let hex = "175948514e4c4f57175b54575b5307515c056a4d0c405901685b500b486075084c09";
        assert_eq!(
            decipher_provider_path(hex).unwrap(),
            "/apivtwo/clock?id=Ru4xa9Pch3pXM0t1"
        );
        assert!(decipher_provider_path("abc").is_err());
        assert!(decipher_provider_path("zz").is_err());
    }

    #[test]
    fn wixmp_variants_expands_urlset() {
        let link = "https://repackager.wixmp.com/video.wixstatic.com/video/abc/,480p,720p,1080p,/mp4/file.mp4.urlset/master.m3u8";
        let vs = wixmp_variants(link).unwrap().unwrap();
        assert_eq!(vs.len(), 3);
        assert_eq!(
            vs[0].url,
            "https://video.wixstatic.com/video/abc/480p/mp4/file.mp4"
        );
        assert_eq!(vs[0].resolution, Some(480));
        assert_eq!(vs[2].resolution, Some(1080));

        assert!(
            wixmp_variants("https://example.com/x.m3u8")
                .unwrap()
                .is_none()
        );
        assert!(wixmp_variants("https://repackager.wixmp.com/no-commas").is_err());
    }

    #[test]
    fn clock_json_inserts_after_clock_segment() {
        assert_eq!(
            clock_json("/apivtwo/clock?id=abc"),
            "/apivtwo/clock.json?id=abc"
        );
        assert_eq!(clock_json("/no/segment/here"), "/no/segment/here");
    }

    #[test]
    fn body_builders_escape_and_stringify_variables() {
        // Search variables are a plain object; episodes/video are stringified
        // JSON with the inner quotes escaped by the outer layer.
        let s = search_body("a\"b", 2, Translation::Sub);
        assert!(s.contains(r#""search":{"query":"a\"b"}"#), "{s}");
        assert!(s.contains(r#""limit":26"#), "{s}");
        assert!(s.contains(r#""translationType":"sub""#), "{s}");
        assert!(s.contains(HASH_SEARCH), "{s}");

        let e = episodes_body("a\"b");
        assert!(e.contains(r#""variables":"{\"_id\":\"a\\\"b\"}""#), "{e}");
        assert!(e.contains(HASH_EPISODES), "{e}");

        let v = video_body("x", Translation::Dub, "1\"");
        assert!(v.contains(r#"\"showId\":\"x\""#), "{v}");
        assert!(v.contains(r#"\"translationType\":\"dub\""#), "{v}");
        assert!(v.contains(r#"\"episodeString\":\"1\\\"\""#), "{v}");
        assert!(v.contains(HASH_VIDEO), "{v}");
    }

    #[test]
    fn cover_request_absolute_passes_relative_gets_cdn() {
        let p = AllAnime::new().unwrap();
        let abs = p
            .cover_request("https://s4.anilist.co/file/x/bx1-abc.jpg")
            .unwrap();
        assert_eq!(abs.url, "https://s4.anilist.co/file/x/bx1-abc.jpg");
        assert!(abs.referer.is_none() && abs.user_agent.is_none());

        let rel = p
            .cover_request("mcovers/a_tbs/dhw/B6AMhLy6EQHDgYgBF.webp")
            .unwrap();
        assert_eq!(
            rel.url,
            format!("{COVER_CDN_BASE}mcovers/a_tbs/dhw/B6AMhLy6EQHDgYgBF.webp")
        );
        assert_eq!(rel.referer.as_deref(), Some(SITE));
        assert_eq!(rel.user_agent.as_deref(), Some(UA));
    }

    #[test]
    fn cover_request_rejects_injection_empty_oversize() {
        let p = AllAnime::new().unwrap();
        assert!(p.cover_request("mcovers/x.webp\r\nX-Injected: 1").is_err());
        assert!(p.cover_request("https://evil/\r\nHost: x").is_err());
        assert!(p.cover_request("mcovers/a b.webp").is_err());
        assert!(p.cover_request("").is_err());
        let oversize = format!("mcovers/{}", "a".repeat(MAX_COVER_REF_LEN + 1));
        assert!(p.cover_request(&oversize).is_err());
    }

    // ── golden fixtures (live capture 2026-07-18, trimmed) ──────────────────

    const SEARCH_FIXTURE: &str = include_str!("../../tests/fixtures/allanime_search.json");
    const EPISODES_FIXTURE: &str = include_str!("../../tests/fixtures/allanime_episodes.json");

    #[test]
    fn golden_search_parses_ranks_and_mines_ids() {
        let hits = AllAnime::parse_search(
            SEARCH_FIXTURE.as_bytes(),
            "sousou no frieren",
            Translation::Sub,
            26,
        )
        .unwrap();
        assert_eq!(hits.len(), 3);

        // Exact title outranks prefix matches regardless of capture order.
        assert_eq!(hits[0].provider_id, "ReHMC7TQnch3C6z8j");
        assert_eq!(hits[0].title, "Sousou no Frieren");
        assert_eq!(hits[0].anilist_id, Some(154587));
        assert_eq!(
            hits[0].title_english.as_deref(),
            Some("Frieren: Beyond Journey\u{2019}s End")
        );
        assert_eq!(hits[0].title_native.as_deref(), Some("葬送のフリーレン"));
        assert_eq!(hits[0].eps_sub, 28);
        assert_eq!(hits[0].eps_dub, 28);
        assert_eq!(hits[0].year, Some(2023));

        // The 'b<digits>' medium-size prefix mines too, not just 'bx'.
        let part3 = hits
            .iter()
            .find(|h| h.provider_id == "ddcCSGNtxd4uLzoxK")
            .unwrap();
        assert_eq!(part3.anilist_id, Some(206425));
        assert_eq!(part3.title_english, None);

        let s2 = hits
            .iter()
            .find(|h| h.provider_id == "qpeexkeTa7DzLjRnp")
            .unwrap();
        assert_eq!(s2.anilist_id, Some(182255));
        assert_eq!(s2.year, Some(2026));
    }

    #[test]
    fn golden_search_respects_limit() {
        let hits =
            AllAnime::parse_search(SEARCH_FIXTURE.as_bytes(), "frieren", Translation::Sub, 1)
                .unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn golden_episodes_sorts_the_unsorted_listing() {
        // The live API returns labels newest-first ("28".."1"); the contract
        // is ascending numeric.
        let eps = AllAnime::parse_episodes(EPISODES_FIXTURE.as_bytes(), Translation::Sub).unwrap();
        assert_eq!(eps.len(), 28);
        assert_eq!(eps.first().map(String::as_str), Some("1"));
        assert_eq!(eps[9], "10");
        assert_eq!(eps.last().map(String::as_str), Some("28"));

        let dub = AllAnime::parse_episodes(EPISODES_FIXTURE.as_bytes(), Translation::Dub).unwrap();
        assert_eq!(dub.len(), 28);
    }

    #[test]
    fn rejected_operation_data_null_is_an_error_not_absence() {
        let got = AllAnime::parse_search(br#"{"data":null}"#, "x", Translation::Sub, 26);
        assert!(matches!(got, Err(ProviderError::Decode(_))));

        let got = AllAnime::parse_episodes(br#"{"data":null}"#, Translation::Sub);
        assert!(matches!(got, Err(ProviderError::Decode(_))));

        // show:null (unknown id) is an error too; only a real empty listing
        // is authoritative not-stocked.
        let got = AllAnime::parse_episodes(br#"{"data":{"show":null}}"#, Translation::Sub);
        assert!(matches!(got, Err(ProviderError::Decode(_))));

        let got = AllAnime::parse_episodes(
            br#"{"data":{"show":{"availableEpisodesDetail":{"sub":[],"dub":["1"]}}}}"#,
            Translation::Sub,
        )
        .unwrap();
        assert!(got.is_empty());
    }

    // ── transport ───────────────────────────────────────────────────────────

    fn against(response: Vec<u8>) -> AllAnime {
        let url = serve_once(response);
        AllAnime::with_endpoints(url.clone(), url).unwrap()
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
    fn transport_resolve_decrypts_and_picks_fast4speed() {
        // Encrypt a payload with the real key so resolve exercises the whole
        // path: POST → tobeparsed → GCM → source gate → StreamLink.
        let plain = r#"{"episode":{"sourceUrls":[{"sourceName":"Default","sourceUrl":"https://tools.fast4speed.rsvp/hls/v.m3u8"}]}}"#;
        let key = Sha256::digest(GCM_SEED);
        let cipher = Aes256Gcm::new_from_slice(key.as_slice()).unwrap();
        let nonce = [7u8; 12];
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), plain.as_bytes())
            .unwrap();
        let mut blob = vec![0u8];
        blob.extend_from_slice(&nonce);
        blob.extend_from_slice(&ct);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&blob);
        let body = format!(r#"{{"data":{{"tobeparsed":"{b64}"}}}}"#);

        let p = against(response_with_body("200 OK", body.as_bytes()));
        let sl = p
            .resolve("id", "1", Translation::Sub, Quality::Best)
            .unwrap();
        assert_eq!(sl.url, "https://tools.fast4speed.rsvp/hls/v.m3u8");
        assert_eq!(sl.resolution, Some(1080));
        assert_eq!(sl.referer.as_deref(), Some(SITE));
    }

    #[test]
    fn transport_resolve_golden_blob_source_fails_the_consider_gate() {
        // The golden vector's sourceUrl has no scheme; consider must refuse
        // to hand it to mpv, and with no long-tail sources resolve errors.
        let body = r#"{"data":{"tobeparsed":"AAABAgMEBQYHCAkKCw/k3QdUZIc5wIflWKnNrBJlDJDvuoUtnAhztwaZ0MPdc+7QLkxnnkAqseAyPNsmcPKDx4IlVT/nzzS1VVCzmf7KRsutWoKHB/11G9S8i9qBiKecETa/9Yrge8E1Rv/TJ35g7iREfYhMrh8s"}}"#;
        let p = against(response_with_body("200 OK", body.as_bytes()));
        let got = p.resolve("id", "1", Translation::Sub, Quality::Best);
        assert!(matches!(got, Err(ProviderError::Decode(_))));
    }
}
