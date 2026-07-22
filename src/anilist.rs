//! AniList GraphQL client: search, discover axes, enrich, list push/pull
//! (06 §8b). The ONLY user-facing catalog client; providers never power
//! Browse/Discover (01 §5). Push/pull land with auth (06 §5). Results land in
//! catalog_cache caller-side; nothing here touches store.

use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::json;

use crate::domain::{Date, Enrichment, ListStatus, Season, current_cour};
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
    // pageInfo is a port adaptation: zigoku search load-more gates next-page
    // on the short-page heuristic (len % 26); explicit hasNextPage matches
    // the discover exhaustion law (ROD-336) instead.
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

// Response DTOs. Fields tolerate null and missing keys (sparse records map to
// sparse Enrichment). A wrong-typed field still fails the whole page: freeze
// parity, any unparseable body is one no-answer. Per-entry salvage is an OPEN
// on the ticket, not a silent upgrade.

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
    page_info: Option<GqlPageInfo>,
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
    id_mal: Option<i64>,
    title: Option<GqlTitle>,
    episodes: Option<u32>,
    duration: Option<u32>,
    average_score: Option<u32>,
    status: Option<String>,
    season: Option<String>,
    season_year: Option<u32>,
    start_date: Option<GqlStartDate>,
    format: Option<String>,
    source: Option<String>,
    country_of_origin: Option<String>,
    genres: Option<Vec<Option<String>>>,
    studios: Option<GqlStudios>,
    rankings: Option<Vec<GqlRanking>>,
    next_airing_episode: Option<GqlNextAiring>,
    description: Option<String>,
    cover_image: Option<GqlCoverImage>,
}

#[derive(Deserialize, Default)]
struct GqlTitle {
    romaji: Option<String>,
    english: Option<String>,
    native: Option<String>,
}

#[derive(Deserialize, Default)]
struct GqlStartDate {
    year: Option<u32>,
    month: Option<u32>,
    day: Option<u32>,
}

#[derive(Deserialize, Default)]
struct GqlStudios {
    nodes: Option<Vec<GqlStudioNode>>,
}

#[derive(Deserialize)]
struct GqlStudioNode {
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlRanking {
    #[serde(default)]
    rank: u32,
    #[serde(rename = "type")]
    kind: Option<String>,
    year: Option<u32>,
    #[serde(default)]
    all_time: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GqlNextAiring {
    episode: Option<u32>,
    airing_at: Option<i64>,
}

#[derive(Deserialize, Default)]
struct GqlCoverImage {
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

/// Fuzzy title score (higher = closer; large negative = no overlap).
/// Normalizes then folds season forms. Shared with resolver tier C (ROD-328)
/// so both match directions use one rule.
pub(crate) fn title_score(a: &str, b: Option<&str>) -> i32 {
    let Some(b) = b else { return -5000 };
    if a.is_empty() || b.is_empty() {
        return -5000;
    }
    let na = canon_season(&normalize_title(a));
    let nb = canon_season(&normalize_title(b));
    if na.is_empty() || nb.is_empty() {
        return -5000;
    }
    if na == nb {
        return 1600;
    }
    if nb.starts_with(&na) || na.starts_with(&nb) {
        return 1250;
    }
    if nb.contains(na.as_str()) || na.contains(nb.as_str()) {
        return 900;
    }
    -5000
}

/// Explicit "Season N" / "Nth Season" in a normalized title → `s<N>`
/// (ROD-181). Bare trailing numbers untouched ("86", "Ranma 1/2"); "season"
/// with no adjacent number is left alone.
fn canon_season(s: &str) -> String {
    let Some(idx) = s.find("season") else {
        return s.to_string();
    };
    let before = &s[..idx];
    let after = &s[idx + "season".len()..];

    // Form A: digits directly after the keyword ("season2").
    let dlen = after.bytes().take_while(u8::is_ascii_digit).count();
    let (base_pre, num, tail) = if dlen > 0 {
        (before, &after[..dlen], &after[dlen..])
    } else {
        // Form B: digits (+ ordinal) directly before the keyword ("2ndseason").
        let mut b = before;
        for ord in ["st", "nd", "rd", "th"] {
            if let Some(stripped) = b.strip_suffix(ord) {
                b = stripped;
                break;
            }
        }
        let digits = b.bytes().rev().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return s.to_string();
        }
        let dstart = b.len() - digits;
        (&b[..dstart], &b[dstart..], after)
    };
    format!("{base_pre}s{num}{tail}")
}

/// ASCII: keep alphanumerics lowercased, drop the rest. Non-ASCII bytes pass
/// verbatim, so multi-byte sequences stay intact.
fn normalize_title(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c < 0x80 {
            if c.is_ascii_alphanumeric() {
                out.push(c.to_ascii_lowercase());
            }
            continue;
        }
        out.push(c);
    }
    String::from_utf8(out).expect("removing whole ascii bytes keeps utf-8 valid")
}

use crate::domain::strip_controls;

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
        cover_url: m.cover_image.and_then(|c| c.large).map(strip_controls),
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

// ---- Auth + sync surface (06 §4.2 Viewer, §5.4 pull, §5.3 push) ----

/// AniList account identity from the `Viewer` query (06 §4.2). The pull needs
/// `id > 0`.
#[derive(Debug, Clone, PartialEq)]
pub struct Viewer {
    pub id: i64,
    pub name: String,
}

/// One remote list entry, mapped to the domain (06 §5.4). `import_seed` carries
/// the media title + episodes so an unmatched WATCHING/REPEATING entry can be
/// auto-imported into the library (06 O3); None when the wire omitted the media
/// node. Reconcile of a matched row never reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteEntry {
    pub anilist_id: i64,
    pub status: ListStatus,
    pub progress: u32,
    pub import_seed: Option<Enrichment>,
}

/// AniList `MediaListStatus` -> domain. REPEATING folds to Watching at ingest so
/// the merge never sees it; unknown/absent -> Planning (06 §5.4).
fn list_status_from_anilist(s: Option<&str>) -> ListStatus {
    match s {
        Some("CURRENT" | "REPEATING") => ListStatus::Watching,
        Some("PLANNING") => ListStatus::Planning,
        Some("PAUSED") => ListStatus::Paused,
        Some("COMPLETED") => ListStatus::Completed,
        Some("DROPPED") => ListStatus::Dropped,
        _ => ListStatus::Planning,
    }
}

/// Domain -> AniList `MediaListStatus` for the push mutation (06 §5.3).
fn list_status_to_anilist(s: ListStatus) -> &'static str {
    match s {
        ListStatus::Watching => "CURRENT",
        ListStatus::Planning => "PLANNING",
        ListStatus::Paused => "PAUSED",
        ListStatus::Completed => "COMPLETED",
        ListStatus::Dropped => "DROPPED",
    }
}

fn viewer_body() -> serde_json::Value {
    json!({ "query": "query{Viewer{id name}}" })
}

fn list_collection_body(user_id: i64) -> serde_json::Value {
    json!({
        "query": "query($userId:Int!){MediaListCollection(userId:$userId,type:ANIME){lists{entries{mediaId status progress media{title{romaji english native} episodes}}}}}",
        "variables": { "userId": user_id },
    })
}

fn save_entry_body(media_id: i64, status: ListStatus, progress: u32) -> serde_json::Value {
    json!({
        "query": "mutation($mediaId:Int!,$status:MediaListStatus!,$progress:Int!){SaveMediaListEntry(mediaId:$mediaId,status:$status,progress:$progress){id}}",
        "variables": {
            "mediaId": media_id,
            "status": list_status_to_anilist(status),
            "progress": progress,
        },
    })
}

#[derive(Deserialize)]
struct ViewerResp {
    data: Option<ViewerData>,
}

#[derive(Deserialize)]
struct ViewerData {
    #[serde(rename = "Viewer")]
    viewer: Option<ViewerNode>,
}

#[derive(Deserialize)]
struct ViewerNode {
    id: i64,
    name: Option<String>,
}

#[derive(Deserialize)]
struct ListCollectionResp {
    data: Option<ListCollectionData>,
}

#[derive(Deserialize)]
struct ListCollectionData {
    #[serde(rename = "MediaListCollection")]
    collection: Option<ListCollection>,
}

#[derive(Deserialize, Default)]
struct ListCollection {
    lists: Option<Vec<ListGroup>>,
}

#[derive(Deserialize, Default)]
struct ListGroup {
    entries: Option<Vec<ListEntryNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListEntryNode {
    media_id: i64,
    status: Option<String>,
    #[serde(default)]
    progress: u32,
    media: Option<ListMediaNode>,
}

#[derive(Deserialize)]
struct ListMediaNode {
    title: Option<GqlTitle>,
    episodes: Option<u32>,
}

#[derive(Deserialize)]
struct SaveResp {
    data: Option<SaveData>,
}

#[derive(Deserialize)]
struct SaveData {
    #[serde(rename = "SaveMediaListEntry")]
    entry: Option<SaveNode>,
}

#[derive(Deserialize)]
struct SaveNode {
    id: Option<i64>,
}

/// Viewer body -> three-state (06 §4.2): `data:null`/garbage = no answer (Err),
/// `Viewer:null` = confirmed rejection (Ok(None)). Persist only on Ok(Some).
fn classify_viewer(raw: &[u8]) -> Result<Option<Viewer>, CatalogError> {
    let resp: ViewerResp =
        serde_json::from_slice(raw).map_err(|e| CatalogError::Decode(e.to_string()))?;
    let data = resp
        .data
        .ok_or_else(|| CatalogError::Decode("data is null".into()))?;
    Ok(data.viewer.map(|v| Viewer {
        id: v.id,
        name: strip_controls(v.name.unwrap_or_default()),
    }))
}

/// Sparse enrichment for auto-importing a list-only show (06 O3): title +
/// episodes only, control-stripped like every render-surface string. The rest
/// of the fields backfill through the TTL enrichment repull.
fn list_import_seed(media_id: i64, m: ListMediaNode) -> Enrichment {
    let title = m.title.unwrap_or_default();
    Enrichment {
        anilist_id: media_id,
        title_romaji: strip_controls_opt(title.romaji).unwrap_or_default(),
        title_english: strip_controls_opt(title.english),
        title_native: strip_controls_opt(title.native),
        total_episodes: m.episodes,
        ..Enrichment::default()
    }
}

/// MediaListCollection body -> flat remote entries. Duplicate ids across custom
/// lists are possible; collapsing them is the reconcile join's job (06 §5.4).
fn classify_list(raw: &[u8]) -> Result<Vec<RemoteEntry>, CatalogError> {
    let resp: ListCollectionResp =
        serde_json::from_slice(raw).map_err(|e| CatalogError::Decode(e.to_string()))?;
    let data = resp
        .data
        .ok_or_else(|| CatalogError::Decode("data is null".into()))?;
    let collection = data.collection.unwrap_or_default();
    let mut out = Vec::new();
    for group in collection.lists.unwrap_or_default() {
        for e in group.entries.unwrap_or_default() {
            let import_seed = e.media.map(|m| list_import_seed(e.media_id, m));
            out.push(RemoteEntry {
                anilist_id: e.media_id,
                status: list_status_from_anilist(e.status.as_deref()),
                progress: e.progress,
                import_seed,
            });
        }
    }
    Ok(out)
}

/// Save body -> the server row id. A 200 without a non-null id is a failure, not
/// a silent success: advancing the snapshot on a phantom save loses the row (06 §5.3).
fn classify_save(raw: &[u8]) -> Result<i64, CatalogError> {
    let resp: SaveResp =
        serde_json::from_slice(raw).map_err(|e| CatalogError::Decode(e.to_string()))?;
    let data = resp
        .data
        .ok_or_else(|| CatalogError::Decode("data is null".into()))?;
    data.entry
        .and_then(|e| e.id)
        .ok_or_else(|| CatalogError::Decode("SaveMediaListEntry.id missing".into()))
}

pub struct AniList {
    http: reqwest::blocking::Client,
    endpoint: String,
}

impl AniList {
    pub fn new() -> Result<AniList, CatalogError> {
        AniList::with_endpoint(ENDPOINT.to_string())
    }

    fn with_endpoint(endpoint: String) -> Result<AniList, CatalogError> {
        // Redirects off: a GraphQL POST to the fixed endpoint has no business
        // redirecting, and following one would hand the request (downgraded
        // to GET by reqwest) to whatever host Location names.
        let http = reqwest::blocking::Client::builder()
            .timeout(DEADLINE)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CatalogError::Network)?;
        Ok(AniList { http, endpoint })
    }

    fn post(&self, body: serde_json::Value) -> Result<Vec<u8>, CatalogError> {
        self.send(body, None)
    }

    /// Bearer variant of [`post`] for the auth/sync calls (06 §4.2/§5). A 401
    /// surfaces as `Http { status: 401 }`; the stop-on-401 policy lives in sync.
    fn post_auth(&self, token: &str, body: serde_json::Value) -> Result<Vec<u8>, CatalogError> {
        self.send(body, Some(token))
    }

    fn send(&self, body: serde_json::Value, token: Option<&str>) -> Result<Vec<u8>, CatalogError> {
        let mut req = self
            .http
            .post(&self.endpoint)
            .header("Accept", "application/json")
            .json(&body);
        if let Some(token) = token {
            req = req.bearer_auth(token);
        }
        let resp = req.send().map_err(|_| CatalogError::Network)?;
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

    /// Verify a token and read the account identity (06 §4.2). Ok(None) =
    /// rejected, Err = no-answer; persist only on Ok(Some).
    pub fn viewer(&self, token: &str) -> Result<Option<Viewer>, CatalogError> {
        classify_viewer(&self.post_auth(token, viewer_body())?)
    }

    /// The full remote list in one unpaginated POST (06 §5.4); a huge list fails
    /// the whole pull (2 MiB cap) rather than truncating.
    pub fn pull_list(&self, token: &str, user_id: i64) -> Result<Vec<RemoteEntry>, CatalogError> {
        classify_list(&self.post_auth(token, list_collection_body(user_id))?)
    }

    /// Push one row (06 §5.3); returns the server row id (never null, see
    /// [`classify_save`]).
    pub fn push_entry(
        &self,
        token: &str,
        media_id: i64,
        status: ListStatus,
        progress: u32,
    ) -> Result<i64, CatalogError> {
        classify_save(&self.post_auth(token, save_entry_body(media_id, status, progress))?)
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

    #[test]
    fn strip_controls_drops_c1_bidi_and_zero_width() {
        assert_eq!(strip_controls("a\u{85}b\u{9F}c".into()), "abc");
        assert_eq!(
            strip_controls("title \u{202E}reversed".into()),
            "title reversed"
        );
        assert_eq!(strip_controls("a\u{2066}b\u{2069}c".into()), "abc");
        assert_eq!(strip_controls("a\u{200B}b\u{FEFF}c".into()), "abc");
    }

    #[test]
    fn title_score_prefers_exact_over_prefix_over_substring() {
        assert!(
            title_score("Frieren", Some("Frieren"))
                > title_score("Frieren", Some("Frieren Season 2"))
        );
        assert!(
            title_score("Frieren", Some("Frieren Season 2"))
                > title_score("Frieren", Some("The World of Frieren"))
        );
        assert_eq!(title_score("Frieren", None), -5000);
        assert_eq!(title_score("", Some("Frieren")), -5000);
        assert_eq!(title_score("Frieren", Some("Naruto")), -5000);
    }

    #[test]
    fn canon_season_reconciles_season_forms_leaves_the_rest() {
        assert_eq!(canon_season("frierenseason2"), "frierens2");
        assert_eq!(canon_season("frieren2ndseason"), "frierens2");
        assert_eq!(canon_season("k3rdseason"), "ks3");
        assert_eq!(canon_season("title2season"), "titles2");
        assert_eq!(canon_season("frieren"), "frieren");
        assert_eq!(canon_season("loghorizon2"), "loghorizon2");
        assert_eq!(canon_season("seasonsoflife"), "seasonsoflife");
    }

    #[test]
    fn title_score_reconciles_season_n_vs_nth_season() {
        assert_eq!(
            title_score(
                "Sousou no Frieren Season 2",
                Some("Sousou no Frieren 2nd Season")
            ),
            1600
        );
        assert!(title_score("Frieren Season 2", Some("Frieren")) < 1600);
    }

    #[test]
    fn normalize_title_lowercases_ascii_keeps_unicode() {
        assert_eq!(normalize_title("Re:Zero 2nd Season"), "rezero2ndseason");
        assert_eq!(normalize_title("葬送のフリーレン"), "葬送のフリーレン");
        assert_eq!(normalize_title("  !!  "), "");
    }

    use crate::testutil::{response_with_body, serve_once, serve_once_capture};

    fn post_against(response: Vec<u8>) -> Result<Vec<u8>, CatalogError> {
        let client = AniList::with_endpoint(serve_once(response)).unwrap();
        client.post(json!({"query": "{}"}))
    }

    #[test]
    fn list_status_maps_both_directions_and_folds_repeating() {
        assert_eq!(
            list_status_from_anilist(Some("CURRENT")),
            ListStatus::Watching
        );
        assert_eq!(
            list_status_from_anilist(Some("REPEATING")),
            ListStatus::Watching
        );
        assert_eq!(
            list_status_from_anilist(Some("PLANNING")),
            ListStatus::Planning
        );
        assert_eq!(list_status_from_anilist(Some("PAUSED")), ListStatus::Paused);
        assert_eq!(
            list_status_from_anilist(Some("COMPLETED")),
            ListStatus::Completed
        );
        assert_eq!(
            list_status_from_anilist(Some("DROPPED")),
            ListStatus::Dropped
        );
        // Unknown/absent never invents an active state.
        assert_eq!(
            list_status_from_anilist(Some("HOARDING")),
            ListStatus::Planning
        );
        assert_eq!(list_status_from_anilist(None), ListStatus::Planning);

        assert_eq!(list_status_to_anilist(ListStatus::Watching), "CURRENT");
        assert_eq!(list_status_to_anilist(ListStatus::Planning), "PLANNING");
        assert_eq!(list_status_to_anilist(ListStatus::Paused), "PAUSED");
        assert_eq!(list_status_to_anilist(ListStatus::Completed), "COMPLETED");
        assert_eq!(list_status_to_anilist(ListStatus::Dropped), "DROPPED");
    }

    #[test]
    fn classify_viewer_three_states() {
        let ok = br#"{"data":{"Viewer":{"id":4242,"name":"rod"}}}"#;
        assert_eq!(
            classify_viewer(ok).unwrap(),
            Some(Viewer {
                id: 4242,
                name: "rod".into()
            })
        );
        // 200 with no Viewer = confirmed rejection.
        assert_eq!(
            classify_viewer(br#"{"data":{"Viewer":null}}"#).unwrap(),
            None
        );
        // data:null / garbage = no answer.
        assert!(classify_viewer(br#"{"data":null}"#).is_err());
        assert!(classify_viewer(b"not json").is_err());
    }

    #[test]
    fn classify_viewer_strips_control_bytes_in_name() {
        let raw = "{\"data\":{\"Viewer\":{\"id\":1,\"name\":\"r\\u0000od\"}}}";
        assert_eq!(
            classify_viewer(raw.as_bytes()).unwrap().unwrap().name,
            "rod"
        );
    }

    #[test]
    fn classify_list_flattens_groups_and_folds_repeating() {
        let raw = br#"{"data":{"MediaListCollection":{"lists":[
            {"entries":[
                {"mediaId":101,"status":"CURRENT","progress":3},
                {"mediaId":102,"status":"REPEATING","progress":12}
            ]},
            {"entries":[
                {"mediaId":103,"status":"COMPLETED","progress":24}
            ]}
        ]}}}"#;
        let got = classify_list(raw).unwrap();
        assert_eq!(
            got,
            vec![
                RemoteEntry {
                    anilist_id: 101,
                    status: ListStatus::Watching,
                    progress: 3,
                    import_seed: None
                },
                RemoteEntry {
                    anilist_id: 102,
                    status: ListStatus::Watching,
                    progress: 12,
                    import_seed: None
                },
                RemoteEntry {
                    anilist_id: 103,
                    status: ListStatus::Completed,
                    progress: 24,
                    import_seed: None
                },
            ]
        );
    }

    #[test]
    fn classify_list_builds_import_seed_from_media() {
        // A control byte in the title proves the seed is stripped like every
        // render-surface string (06 O3).
        let raw = "{\"data\":{\"MediaListCollection\":{\"lists\":[{\"entries\":[
            {\"mediaId\":101,\"status\":\"CURRENT\",\"progress\":3,\"media\":{\"title\":{\"romaji\":\"Fr\\u0000ieren\",\"english\":\"Frieren\",\"native\":\"\u{846c}\u{9001}\u{306e}\u{30d5}\u{30ea}\u{30fc}\u{30ec}\u{30f3}\"},\"episodes\":28}}
        ]}]}}}";
        let seed = classify_list(raw.as_bytes()).unwrap()[0]
            .import_seed
            .clone()
            .expect("media present -> seed built");
        assert_eq!(seed.anilist_id, 101);
        assert_eq!(seed.title_romaji, "Frieren");
        assert_eq!(seed.title_english.as_deref(), Some("Frieren"));
        assert_eq!(seed.total_episodes, Some(28));
        // Untouched by the seed builder; backfills via TTL repull.
        assert_eq!(seed.cover_url, None);
    }

    #[test]
    fn classify_list_empty_and_null_cases() {
        // No lists at all is a clean empty pull, not an error.
        assert!(
            classify_list(br#"{"data":{"MediaListCollection":{"lists":[]}}}"#)
                .unwrap()
                .is_empty()
        );
        assert!(
            classify_list(br#"{"data":{"MediaListCollection":null}}"#)
                .unwrap()
                .is_empty()
        );
        // Missing progress defaults to 0; unknown status degrades to Planning.
        let sparse = br#"{"data":{"MediaListCollection":{"lists":[{"entries":[{"mediaId":9}]}]}}}"#;
        assert_eq!(
            classify_list(sparse).unwrap(),
            vec![RemoteEntry {
                anilist_id: 9,
                status: ListStatus::Planning,
                progress: 0,
                import_seed: None
            }]
        );
        assert!(classify_list(br#"{"data":null}"#).is_err());
    }

    #[test]
    fn classify_save_requires_non_null_id() {
        assert_eq!(
            classify_save(br#"{"data":{"SaveMediaListEntry":{"id":55123}}}"#).unwrap(),
            55123
        );
        // 200 with a null id is a failure, never a silent success.
        assert!(classify_save(br#"{"data":{"SaveMediaListEntry":{"id":null}}}"#).is_err());
        assert!(classify_save(br#"{"data":{"SaveMediaListEntry":null}}"#).is_err());
        assert!(classify_save(br#"{"data":null}"#).is_err());
    }

    #[test]
    fn viewer_sends_the_bearer_and_parses() {
        let (url, rx) = serve_once_capture(response_with_body(
            "200 OK",
            br#"{"data":{"Viewer":{"id":7,"name":"rod"}}}"#,
        ));
        let client = AniList::with_endpoint(url).unwrap();
        let v = client.viewer("secret-token-value").unwrap().unwrap();
        assert_eq!(
            v,
            Viewer {
                id: 7,
                name: "rod".into()
            }
        );

        let raw = rx.recv().unwrap();
        let req = String::from_utf8_lossy(&raw);
        assert!(
            req.contains("authorization: Bearer secret-token-value")
                || req.contains("Authorization: Bearer secret-token-value"),
            "bearer not sent; request head was:\n{req}"
        );
    }

    #[test]
    fn push_entry_sends_status_and_returns_id() {
        let (url, rx) = serve_once_capture(response_with_body(
            "200 OK",
            br#"{"data":{"SaveMediaListEntry":{"id":900}}}"#,
        ));
        let client = AniList::with_endpoint(url).unwrap();
        let id = client
            .push_entry("tok", 154587, ListStatus::Watching, 5)
            .unwrap();
        assert_eq!(id, 900);

        let raw = rx.recv().unwrap();
        let req = String::from_utf8_lossy(&raw);
        // The body rides the same request; the domain status maps to CURRENT.
        assert!(
            req.contains("CURRENT"),
            "status not mapped; body was:\n{req}"
        );
        assert!(req.contains("154587"));
    }

    #[test]
    fn transport_401_is_http_status_for_the_sync_layer() {
        // The auth calls lean on Http{401} to mean "stop the run" (06 §5.3).
        let got = post_against(response_with_body("401 Unauthorized", b"{}"));
        assert!(matches!(got, Err(CatalogError::Http { status: 401 })));
    }

    #[test]
    fn transport_429_is_rate_limited() {
        let got = post_against(response_with_body("429 Too Many Requests", b"{}"));
        assert!(matches!(got, Err(CatalogError::RateLimited)));
    }

    #[test]
    fn transport_non_success_is_http_status() {
        let got = post_against(response_with_body("500 Internal Server Error", b"{}"));
        assert!(matches!(got, Err(CatalogError::Http { status: 500 })));
    }

    #[test]
    fn transport_redirect_is_refused_not_followed() {
        let redirect = b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec();
        let got = post_against(redirect);
        assert!(matches!(got, Err(CatalogError::Http { status: 302 })));
    }

    #[test]
    fn transport_body_at_cap_is_accepted_one_over_is_refused() {
        let at_cap = vec![b'x'; MAX_RESPONSE_BYTES as usize];
        let got = post_against(response_with_body("200 OK", &at_cap)).unwrap();
        assert_eq!(got.len() as u64, MAX_RESPONSE_BYTES);

        let over = vec![b'x'; MAX_RESPONSE_BYTES as usize + 1];
        let got = post_against(response_with_body("200 OK", &over));
        assert!(matches!(got, Err(CatalogError::Decode(_))));
    }
}
