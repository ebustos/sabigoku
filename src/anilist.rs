//! AniList GraphQL client: search, discover axes, enrich, list push/pull
//! (06 §8b). The ONLY user-facing catalog client; providers never power
//! Browse/Discover (01 §5). Push/pull land with auth (06 §5). Results land in
//! catalog_cache caller-side; nothing here touches store.

use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::json;

use crate::domain::{Date, Enrichment, Season, current_cour};
use crate::providers::{
    CatalogError, CatalogPage, CatalogProvider, DISCOVER_PAGE_SIZE, DiscoverAxis, SEARCH_PAGE_SIZE,
};

const ENDPOINT: &str = "https://graphql.anilist.co";
/// Wall-clock ceiling per POST (ROD-262): side rail, one round trip; a silent
/// host must not hang a detached worker.
const DEADLINE: Duration = Duration::from_secs(10);
/// Fixed response cap (ROD-247): unbounded read could OOM; real replies << 100 KB.
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;

/// Shared selection set so by-id, search, and discover never drift (06 §8b:
/// one fieldset; card vs detail is UI-side selection).
const MEDIA_FIELDS: &str = "id idMal title{romaji english native} episodes duration averageScore status season seasonYear startDate{year month day} format source countryOfOrigin genres studios(isMain:true){nodes{name}} rankings{rank type year allTime} nextAiringEpisode{episode airingAt} description(asHtml:false) coverImage{large}";

fn search_query() -> String {
    // pageInfo is a port adaptation: sabigoku Browse load-more is AniList-fed
    // (zigoku's was provider-fed), so search needs hasNextPage too.
    format!(
        "query($search:String!,$perPage:Int!,$page:Int!){{Page(page:$page,perPage:$perPage){{pageInfo{{hasNextPage}} media(search:$search,type:ANIME,sort:SEARCH_MATCH){{{MEDIA_FIELDS}}}}}}}"
    )
}

fn discover_query() -> String {
    format!(
        "query($page:Int!,$perPage:Int!,$sort:[MediaSort],$season:MediaSeason,$seasonYear:Int){{Page(page:$page,perPage:$perPage){{pageInfo{{hasNextPage}} media(type:ANIME,sort:$sort,season:$season,seasonYear:$seasonYear){{{MEDIA_FIELDS}}}}}}}"
    )
}

/// Deterministic join when the AniList id is known (ROD-181); no title match.
fn by_id_query() -> String {
    format!("query($id:Int!){{Media(id:$id,type:ANIME){{{MEDIA_FIELDS}}}}}")
}

/// Secondary sort key stabilizes page order under primary ties (ROD-334 §9.6).
fn sort_keys(axis: DiscoverAxis) -> &'static [&'static str] {
    match axis {
        DiscoverAxis::Trending => &["TRENDING_DESC", "POPULARITY_DESC"],
        DiscoverAxis::Popular | DiscoverAxis::ThisSeason => &["POPULARITY_DESC", "ID_DESC"],
        DiscoverAxis::TopRated => &["SCORE_DESC", "ID_DESC"],
    }
}

fn season_gql(s: Season) -> &'static str {
    match s {
        Season::Winter => "WINTER",
        Season::Spring => "SPRING",
        Season::Summer => "SUMMER",
        Season::Fall => "FALL",
    }
}

fn search_body(query_text: &str, page: u32) -> serde_json::Value {
    json!({
        "query": search_query(),
        "variables": { "search": query_text, "perPage": SEARCH_PAGE_SIZE, "page": page },
    })
}

fn discover_body(axis: DiscoverAxis, page: u32, unix_secs: i64) -> serde_json::Value {
    let mut vars = json!({
        "page": page,
        "perPage": DISCOVER_PAGE_SIZE,
        "sort": sort_keys(axis),
    });
    // season/seasonYear OMITTED off This Season: an explicit null would filter
    // season==null; omitted leaves the GraphQL arg unset (06 §8b).
    if axis == DiscoverAxis::ThisSeason {
        let c = current_cour(unix_secs);
        vars["season"] = json!(season_gql(c.season));
        vars["seasonYear"] = json!(c.year);
    }
    json!({ "query": discover_query(), "variables": vars })
}

fn by_id_body(anilist_id: i64) -> serde_json::Value {
    json!({ "query": by_id_query(), "variables": { "id": anilist_id } })
}

// Response DTOs. Every field tolerates null AND missing: a partial record must
// degrade to sparse Enrichment, never fail the whole page.

#[derive(Deserialize)]
struct GqlPageResp {
    data: Option<GqlPageData>,
}

#[derive(Deserialize)]
struct GqlPageData {
    #[serde(rename = "Page")]
    page: Option<GqlPage>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct GqlPage {
    #[serde(default)]
    page_info: Option<GqlPageInfo>,
    #[serde(default)]
    media: Option<Vec<GqlMedia>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlPageInfo {
    #[serde(default)]
    has_next_page: bool,
}

#[derive(Deserialize)]
struct GqlMediaResp {
    data: Option<GqlMediaData>,
}

#[derive(Deserialize)]
struct GqlMediaData {
    #[serde(rename = "Media")]
    media: Option<GqlMedia>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlMedia {
    id: i64,
    #[serde(default)]
    id_mal: Option<i64>,
    #[serde(default)]
    title: Option<GqlTitle>,
    #[serde(default)]
    episodes: Option<u32>,
    #[serde(default)]
    duration: Option<u32>,
    #[serde(default)]
    average_score: Option<u32>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    season: Option<String>,
    #[serde(default)]
    season_year: Option<u32>,
    #[serde(default)]
    start_date: Option<GqlStartDate>,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    country_of_origin: Option<String>,
    #[serde(default)]
    genres: Option<Vec<Option<String>>>,
    #[serde(default)]
    studios: Option<GqlStudios>,
    #[serde(default)]
    rankings: Option<Vec<GqlRanking>>,
    #[serde(default)]
    next_airing_episode: Option<GqlNextAiring>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    cover_image: Option<GqlCoverImage>,
}

#[derive(Deserialize, Default)]
struct GqlTitle {
    #[serde(default)]
    romaji: Option<String>,
    #[serde(default)]
    english: Option<String>,
    #[serde(default)]
    native: Option<String>,
}

#[derive(Deserialize, Default)]
struct GqlStartDate {
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    month: Option<u32>,
    #[serde(default)]
    day: Option<u32>,
}

#[derive(Deserialize, Default)]
struct GqlStudios {
    #[serde(default)]
    nodes: Option<Vec<GqlStudioNode>>,
}

#[derive(Deserialize)]
struct GqlStudioNode {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlRanking {
    #[serde(default)]
    rank: u32,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    year: Option<u32>,
    #[serde(default)]
    all_time: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlNextAiring {
    #[serde(default)]
    episode: Option<u32>,
    #[serde(default)]
    airing_at: Option<i64>,
}

#[derive(Deserialize, Default)]
struct GqlCoverImage {
    #[serde(default)]
    large: Option<String>,
}

struct SelectedRank {
    rank: u32,
    kind: Option<String>,
    year: Option<u32>,
}

/// Best ranking (ROD-261 §5.3a): contextual over all-time; within a tier
/// RATED over POPULAR. Strict greater keeps the first on ties.
fn select_rank(rankings: &[GqlRanking]) -> Option<SelectedRank> {
    fn score(r: &GqlRanking) -> i32 {
        let mut s = 0;
        if !r.all_time {
            s += 2;
        }
        if r.kind.as_deref() == Some("RATED") {
            s += 1;
        }
        s
    }
    let mut best: Option<&GqlRanking> = None;
    for r in rankings {
        if best.is_none_or(|b| score(r) > score(b)) {
            best = Some(r);
        }
    }
    let b = best?;
    Some(SelectedRank {
        rank: b.rank,
        kind: b.kind.clone(),
        year: if b.all_time { None } else { b.year },
    })
}

/// Drop C0 + DEL from AniList free text before it can reach terminal cells
/// (ROD-247). Explicit defense, not left to the render layer.
fn strip_controls(s: String) -> String {
    if s.bytes().any(|b| b < 0x20 || b == 0x7F) {
        s.chars()
            .filter(|&c| c >= '\u{20}' && c != '\u{7F}')
            .collect()
    } else {
        s
    }
}

fn strip_controls_opt(s: Option<String>) -> Option<String> {
    s.map(strip_controls)
}

/// `description(asHtml:false)` still carries tags and entities: strip tags,
/// decode the common entities, collapse newline runs to one space, trim.
fn sanitize_description(raw: &str) -> String {
    const ENTITIES: [(&[u8], &[u8]); 7] = [
        (b"&amp;", b"&"),
        (b"&quot;", b"\""),
        (b"&#039;", b"'"),
        (b"&lt;", b"<"),
        (b"&gt;", b">"),
        (b"&mdash;", b"--"),
        (b"&ndash;", b"-"),
    ];
    let b = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    let mut in_tag = false;
    while i < b.len() {
        let c = b[i];
        if in_tag {
            if c == b'>' {
                in_tag = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'<' => {
                in_tag = true;
                i += 1;
            }
            b'&' => {
                if let Some((pat, rep)) = ENTITIES.iter().find(|(p, _)| b[i..].starts_with(p)) {
                    out.extend_from_slice(rep);
                    i += pat.len();
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            b'\n' | b'\r' | b'\t' => {
                if out.last().is_some_and(|&l| l != b' ') {
                    out.push(b' ');
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).trim_matches(' ').to_string()
}

fn media_to_enrichment(m: GqlMedia) -> Enrichment {
    let title = m.title.unwrap_or_default();
    let sel = select_rank(m.rankings.as_deref().unwrap_or_default());
    let (next_at, next_ep) = m
        .next_airing_episode
        .map_or((None, None), |na| (na.airing_at, na.episode));
    Enrichment {
        anilist_id: m.id,
        mal_id: m.id_mal,
        // Blank romaji is absence; the store merge NULLIFs it (02 §merge).
        title_romaji: strip_controls_opt(title.romaji).unwrap_or_default(),
        title_english: strip_controls_opt(title.english),
        title_native: strip_controls_opt(title.native),
        cover_url: m.cover_image.and_then(|c| c.large),
        total_episodes: m.episodes,
        duration_minutes: m.duration,
        year: m.season_year,
        season: m.season.as_deref().and_then(Season::parse),
        status: strip_controls_opt(m.status),
        description: m
            .description
            .map(|d| strip_controls(sanitize_description(&d))),
        score: m.average_score,
        kind: strip_controls_opt(m.format),
        start_date: m.start_date.and_then(|sd| {
            sd.year.map(|year| Date {
                year,
                month: sd.month,
                day: sd.day,
            })
        }),
        genres: m
            .genres
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .map(strip_controls)
            .collect(),
        studios: m
            .studios
            .and_then(|s| s.nodes)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|n| n.name)
            .map(strip_controls)
            .collect(),
        source_material: strip_controls_opt(m.source),
        rank: sel.as_ref().map(|r| r.rank),
        rank_type: sel
            .as_ref()
            .and_then(|r| r.kind.clone())
            .map(strip_controls),
        rank_year: sel.as_ref().and_then(|r| r.year),
        next_airing_at: next_at,
        next_airing_episode: next_ep,
        country: strip_controls_opt(m.country_of_origin),
    }
}

/// Page body → page. Unparseable / data:null = no answer; missing Page or
/// pageInfo = exhausted (stop, don't spin).
fn classify_page(raw: &[u8]) -> Result<CatalogPage, CatalogError> {
    let resp: GqlPageResp =
        serde_json::from_slice(raw).map_err(|e| CatalogError::Decode(e.to_string()))?;
    let data = resp
        .data
        .ok_or_else(|| CatalogError::Decode("data is null".into()))?;
    let page = data.page.unwrap_or_default();
    Ok(CatalogPage {
        entries: page
            .media
            .unwrap_or_default()
            .into_iter()
            .map(media_to_enrichment)
            .collect(),
        has_next: page.page_info.is_some_and(|pi| pi.has_next_page),
    })
}

/// By-id body → three-state (05 §8): unparseable / data:null = no answer
/// (Err), Media:null = confirmed no-match (Ok(None)).
fn classify_by_id(raw: &[u8]) -> Result<Option<Enrichment>, CatalogError> {
    let resp: GqlMediaResp =
        serde_json::from_slice(raw).map_err(|e| CatalogError::Decode(e.to_string()))?;
    let data = resp
        .data
        .ok_or_else(|| CatalogError::Decode("data is null".into()))?;
    Ok(data.media.map(media_to_enrichment))
}

pub struct AniList {
    http: reqwest::blocking::Client,
}

impl AniList {
    pub fn new() -> Result<AniList, CatalogError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(DEADLINE)
            .build()
            .map_err(|_| CatalogError::Network)?;
        Ok(AniList { http })
    }

    fn post(&self, body: serde_json::Value) -> Result<Vec<u8>, CatalogError> {
        let resp = self
            .http
            .post(ENDPOINT)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .map_err(|_| CatalogError::Network)?;
        let status = resp.status().as_u16();
        if status == 429 {
            return Err(CatalogError::RateLimited);
        }
        if !resp.status().is_success() {
            return Err(CatalogError::Http { status });
        }
        let mut buf = Vec::new();
        resp.take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut buf)
            .map_err(|_| CatalogError::Network)?;
        if buf.len() as u64 > MAX_RESPONSE_BYTES {
            return Err(CatalogError::Decode(
                "response exceeds the 2 MiB cap".into(),
            ));
        }
        Ok(buf)
    }
}

impl CatalogProvider for AniList {
    fn search(&self, query: &str, page: u32) -> Result<CatalogPage, CatalogError> {
        classify_page(&self.post(search_body(query, page))?)
    }

    fn discover(&self, axis: DiscoverAxis, page: u32) -> Result<CatalogPage, CatalogError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        classify_page(&self.post(discover_body(axis, page, now))?)
    }

    fn enrich(&self, anilist_id: i64) -> Result<Option<Enrichment>, CatalogError> {
        classify_by_id(&self.post(by_id_body(anilist_id))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH_FIXTURE: &str = include_str!("../tests/fixtures/anilist_search.json");
    const DISCOVER_FIXTURE: &str = include_str!("../tests/fixtures/anilist_discover.json");
    const BY_ID_FIXTURE: &str = include_str!("../tests/fixtures/anilist_by_id.json");

    #[test]
    fn golden_by_id_full_mapping() {
        let e = classify_by_id(BY_ID_FIXTURE.as_bytes()).unwrap().unwrap();
        assert_eq!(e.anilist_id, 154587);
        assert_eq!(e.mal_id, Some(52991));
        assert_eq!(e.title_romaji, "Sousou no Frieren");
        assert_eq!(
            e.title_english.as_deref(),
            Some("Frieren: Beyond Journey\u{2019}s End")
        );
        assert_eq!(e.title_native.as_deref(), Some("葬送のフリーレン"));
        assert_eq!(e.total_episodes, Some(28));
        assert_eq!(e.duration_minutes, Some(24));
        assert_eq!(e.year, Some(2023));
        assert_eq!(e.season, Some(Season::Fall));
        assert_eq!(e.status.as_deref(), Some("FINISHED"));
        assert_eq!(e.score, Some(91));
        assert_eq!(e.kind.as_deref(), Some("TV"));
        assert_eq!(
            e.start_date,
            Some(Date {
                year: 2023,
                month: Some(9),
                day: Some(29)
            })
        );
        assert_eq!(e.genres, vec!["Adventure", "Drama", "Fantasy"]);
        assert_eq!(e.studios, vec!["MADHOUSE"]);
        assert_eq!(e.source_material.as_deref(), Some("MANGA"));
        // Contextual RATED 2023 outranks the all-time rank 1 (ROD-261 §5.3a).
        assert_eq!(e.rank, Some(1));
        assert_eq!(e.rank_type.as_deref(), Some("RATED"));
        assert_eq!(e.rank_year, Some(2023));
        assert_eq!(e.next_airing_at, None);
        assert_eq!(e.next_airing_episode, None);
        assert_eq!(e.country.as_deref(), Some("JP"));
        let cover = e.cover_url.unwrap();
        assert!(cover.starts_with("https://") && cover.contains("154587"));
        let desc = e.description.unwrap();
        assert!(desc.ends_with("(Source: Crunchyroll)"));
        assert!(!desc.contains('<') && !desc.contains('\n'));
    }

    #[test]
    fn golden_search_page() {
        let page = classify_page(SEARCH_FIXTURE.as_bytes()).unwrap();
        assert_eq!(page.entries.len(), 3);
        assert!(page.has_next);
        assert_eq!(page.entries[0].anilist_id, 154587);
        assert_eq!(page.entries[1].title_romaji, "Sousou no Frieren 3rd Season");
        // Unaired sequel: sparse record must map, not fail (episodes null).
        assert_eq!(page.entries[1].total_episodes, None);
    }

    #[test]
    fn golden_discover_page() {
        let page = classify_page(DISCOVER_FIXTURE.as_bytes()).unwrap();
        assert_eq!(page.entries.len(), 3);
        assert!(page.has_next);
        for e in &page.entries {
            assert!(e.anilist_id > 0);
            assert!(!e.title_romaji.is_empty());
            assert!(e.cover_url.is_some());
        }
        // Airing shows carry the countdown pair.
        assert!(page.entries[0].next_airing_at.is_some());
        assert!(page.entries[0].next_airing_episode.is_some());
    }

    #[test]
    fn by_id_media_null_is_confirmed_no_match() {
        let raw = br#"{"data":{"Media":null}}"#;
        assert!(classify_by_id(raw).unwrap().is_none());
    }

    #[test]
    fn data_null_is_no_answer() {
        let raw = br#"{"data":null}"#;
        assert!(classify_by_id(raw).is_err());
        assert!(classify_page(raw).is_err());
        assert!(classify_page(b"not json").is_err());
    }

    #[test]
    fn page_without_pageinfo_is_exhausted() {
        let raw = br#"{"data":{"Page":{"media":[]}}}"#;
        let page = classify_page(raw).unwrap();
        assert!(page.entries.is_empty());
        assert!(!page.has_next);
    }

    #[test]
    fn discover_body_omits_season_vars_off_this_season() {
        let body = discover_body(DiscoverAxis::Trending, 1, 1_784_332_800);
        let vars = &body["variables"];
        assert!(vars.get("season").is_none());
        assert!(vars.get("seasonYear").is_none());
        assert_eq!(vars["sort"], json!(["TRENDING_DESC", "POPULARITY_DESC"]));
        assert_eq!(vars["perPage"], json!(DISCOVER_PAGE_SIZE));
    }

    #[test]
    fn discover_body_this_season_binds_current_cour() {
        // 2026-07-18: summer 2026.
        let body = discover_body(DiscoverAxis::ThisSeason, 2, 1_784_332_800);
        let vars = &body["variables"];
        assert_eq!(vars["season"], json!("SUMMER"));
        assert_eq!(vars["seasonYear"], json!(2026));
        assert_eq!(vars["sort"], json!(["POPULARITY_DESC", "ID_DESC"]));
        assert_eq!(vars["page"], json!(2));
    }

    #[test]
    fn search_body_binds_browse_page_size() {
        let body = search_body("frieren", 1);
        assert_eq!(body["variables"]["perPage"], json!(SEARCH_PAGE_SIZE));
        assert_eq!(body["variables"]["search"], json!("frieren"));
    }

    #[test]
    fn select_rank_prefers_contextual_rated() {
        let rankings = vec![
            GqlRanking {
                rank: 1,
                kind: Some("RATED".into()),
                year: None,
                all_time: true,
            },
            GqlRanking {
                rank: 7,
                kind: Some("POPULAR".into()),
                year: Some(2024),
                all_time: false,
            },
            GqlRanking {
                rank: 3,
                kind: Some("RATED".into()),
                year: Some(2024),
                all_time: false,
            },
        ];
        let sel = select_rank(&rankings).unwrap();
        assert_eq!(sel.rank, 3);
        assert_eq!(sel.kind.as_deref(), Some("RATED"));
        assert_eq!(sel.year, Some(2024));
    }

    #[test]
    fn select_rank_all_time_year_is_none() {
        let rankings = vec![GqlRanking {
            rank: 5,
            kind: Some("POPULAR".into()),
            year: Some(2020),
            all_time: true,
        }];
        let sel = select_rank(&rankings).unwrap();
        assert_eq!(sel.year, None);
        assert!(select_rank(&[]).is_none());
    }

    #[test]
    fn select_rank_tie_keeps_first() {
        let rankings = vec![
            GqlRanking {
                rank: 10,
                kind: Some("POPULAR".into()),
                year: Some(2024),
                all_time: false,
            },
            GqlRanking {
                rank: 20,
                kind: Some("POPULAR".into()),
                year: Some(2023),
                all_time: false,
            },
        ];
        assert_eq!(select_rank(&rankings).unwrap().rank, 10);
    }

    #[test]
    fn sanitize_description_strips_tags_entities_newlines() {
        assert_eq!(
            sanitize_description("<i>Foo</i>\nbar &amp; baz&#039;s &mdash; ok\n<br><br>\n(end)"),
            "Foo bar & baz's -- ok (end)"
        );
        assert_eq!(sanitize_description("a &unknown; b"), "a &unknown; b");
        assert_eq!(sanitize_description("\n\t x \n"), "x");
    }

    #[test]
    fn strip_controls_drops_c0_and_del_keeps_unicode() {
        assert_eq!(strip_controls("a\x00b\x7fc".into()), "abc");
        assert_eq!(
            strip_controls("葬送のフリーレン".into()),
            "葬送のフリーレン"
        );
    }
}
