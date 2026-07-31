//! epeng.animeapps.top `StreamProvider` (ROD-515). Tier-A, AniList-keyed:
//! `api2.php?epid={anilistId}`. Pure JSON catalog, no search endpoint, no
//! crypto. Two player types behind apilink.php: play2.php (hardsub) and
//! playsub.php (softsub with jwplayer-style VTT tracks).
//!
//! Resolve chain: api2.php (catalog) -> apilink.php (player links) -> player
//! page HTML -> regex videoUrl -> HLS master. Referer + UA required.

use serde::Deserialize;

use super::http::{Accept, HttpClient, Method, Request};
use crate::domain::{Enrichment, Quality, StreamLink, Translation, is_absolute_url};
use crate::fetchguard::guard_fetch_url;
use crate::providers::{
    CoverRequest, ProviderError, SearchHit, SearchOptions, StreamProvider, clean_arg, guard_show_id,
};

const API: &str = "https://epeng.animeapps.top";
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";
const REFERER: &str = "https://epeng.animeapps.top/";
// Player pages tried per resolve before giving up.
const MAX_PLAYER_TRIES: usize = 3;

// -- catalog DTOs (api2.php) ------------------------------------------------

#[derive(Deserialize)]
struct ServerGroup {
    server_name: Option<String>,
    #[serde(default)]
    server_data: Vec<EpisodeEntry>,
}

#[derive(Deserialize)]
struct EpisodeEntry {
    name: Option<String>,
    slug: Option<String>,
    link: Option<String>,
}

// -- player link DTOs (apilink.php) -----------------------------------------

#[derive(Deserialize)]
struct PlayerEntry {
    link: Option<String>,
}

struct CatalogGroup {
    audio: &'static str,
    episodes: Vec<(u32, String)>, // (number, link)
}

// -- pure parsers -----------------------------------------------------------

/// "dub" if server_name contains "dub" (case-insensitive), else "sub".
fn audio_kind(server_name: Option<&str>) -> &'static str {
    match server_name {
        Some(s) if s.to_ascii_lowercase().contains("dub") => "dub",
        _ => "sub",
    }
}

/// Parse the catalog response into typed groups.
fn parse_catalog(raw: &[u8]) -> Result<Vec<CatalogGroup>, ProviderError> {
    let groups: Vec<ServerGroup> =
        serde_json::from_slice(raw).map_err(|e| ProviderError::Decode(format!("catalog: {e}")))?;
    Ok(groups
        .into_iter()
        .filter_map(|g| {
            let audio = audio_kind(g.server_name.as_deref());
            let episodes: Vec<(u32, String)> = g
                .server_data
                .into_iter()
                .filter_map(|e| {
                    let num = parse_ep_number(&e)?;
                    let link = e.link.filter(|l| !l.is_empty())?;
                    Some((num, link))
                })
                .collect();
            if episodes.is_empty() {
                None
            } else {
                Some(CatalogGroup { audio, episodes })
            }
        })
        .collect())
}

/// Episode number from name (preferred) or slug. Leading zeros stripped by
/// the u32 parse. Zero and negative rejected.
fn parse_ep_number(entry: &EpisodeEntry) -> Option<u32> {
    entry
        .name
        .as_deref()
        .or(entry.slug.as_deref())
        .and_then(|s| s.parse::<u32>().ok())
        .filter(|&n| n >= 1)
}

/// Sorted, deduplicated episode labels across all audio kinds.
fn episode_labels(groups: &[CatalogGroup]) -> Vec<String> {
    let mut nums: Vec<u32> = groups
        .iter()
        .flat_map(|g| g.episodes.iter().map(|(n, _)| *n))
        .collect();
    nums.sort_unstable();
    nums.dedup();
    nums.into_iter().map(|n| n.to_string()).collect()
}

/// Find the link for a specific episode + translation in parsed groups.
fn find_link(groups: &[CatalogGroup], episode: u32, tt: Translation) -> Option<&str> {
    let want = match tt {
        Translation::Sub => "sub",
        Translation::Dub => "dub",
    };
    groups
        .iter()
        .filter(|g| g.audio == want)
        .flat_map(|g| &g.episodes)
        .find(|(n, _)| *n == episode)
        .map(|(_, link)| link.as_str())
}

/// Parse player entries from apilink.php response.
fn parse_players(raw: &[u8]) -> Result<Vec<PlayerEntry>, ProviderError> {
    serde_json::from_slice(raw).map_err(|e| ProviderError::Decode(format!("players: {e}")))
}

/// Extract `videoUrl: "..."` from an ArtPlayer config in player HTML.
/// Relative paths are resolved against `origin`.
fn extract_video_url(html: &str, origin: &str) -> Option<String> {
    let needle = "videoUrl";
    let at = html.find(needle)?;
    let rest = &html[at + needle.len()..];
    // Skip whitespace and colon.
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':')?;
    let rest = rest.trim_start();
    // Quoted value.
    let quote = rest.as_bytes().first()?;
    if *quote != b'"' && *quote != b'\'' {
        return None;
    }
    let inner = &rest[1..];
    let end = inner.find(*quote as char)?;
    let raw = &inner[..end];
    if raw.is_empty() {
        return None;
    }
    Some(resolve_url(raw, origin))
}

/// Resolve a potentially relative URL against an origin.
fn resolve_url(raw: &str, origin: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else if raw.starts_with('/') {
        format!("{origin}{raw}")
    } else {
        format!("{origin}/{raw}")
    }
}

/// Extract the origin (scheme + authority) from a URL.
fn url_origin(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => parsed.origin().ascii_serialization(),
        Err(_) => url.to_string(),
    }
}

/// Parse jwplayer-style soft subtitles from player HTML (playsub.php).
/// `tracks: [{...}, ...]` where each object has file, label, kind fields.
/// Naive brace scanner: breaks if a string value contains `}` or `]`.
fn parse_subtitles(html: &str) -> Vec<SubTrack> {
    // Find the tracks array.
    let Some(at) = html.find("tracks") else {
        return Vec::new();
    };
    let rest = &html[at + "tracks".len()..];
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix(':') else {
        return Vec::new();
    };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix('[') else {
        return Vec::new();
    };
    let end = match rest.find(']') {
        Some(i) => i,
        None => return Vec::new(),
    };
    let block = &rest[..end];

    // Parse individual objects.
    let mut tracks = Vec::new();
    let mut pos = 0;
    while pos < block.len() {
        let Some(obj_start) = block[pos..].find('{') else {
            break;
        };
        let obj_start = pos + obj_start;
        let Some(obj_end) = block[obj_start..].find('}') else {
            break;
        };
        let obj = &block[obj_start..obj_start + obj_end + 1];
        if let Some(track) = parse_sub_track(obj) {
            tracks.push(track);
        }
        pos = obj_start + obj_end + 1;
    }
    tracks
}

struct SubTrack {
    file: String,
    label: String,
}

/// Parse one `{...}` object from the tracks array.
fn parse_sub_track(obj: &str) -> Option<SubTrack> {
    let file = json_string_field(obj, "file")?;
    if !file.starts_with("http") {
        return None;
    }
    // Only captions/subtitles, not thumbnails.
    if let Some(kind) = json_string_field(obj, "kind")
        && kind != "captions"
        && kind != "subtitles"
    {
        return None;
    }
    let label = json_string_field(obj, "label")
        .filter(|l| !l.is_empty())
        .unwrap_or_else(|| "Subtitle".to_string());
    Some(SubTrack { file, label })
}

/// Extract a JSON string field value: `"name": "value"` -> `value`.
fn json_string_field(obj: &str, name: &str) -> Option<String> {
    let needle = format!("\"{name}\"");
    let at = obj.find(&needle)?;
    let rest = &obj[at + needle.len()..];
    let rest = rest.trim_start();
    let rest = rest.strip_prefix(':')?;
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    let val = &rest[..end];
    if val.is_empty() {
        return None;
    }
    Some(val.to_string())
}

/// Pick the best subtitle: English-labeled preferred, then first.
fn pick_subtitle(tracks: &[SubTrack]) -> Option<&SubTrack> {
    let english = tracks
        .iter()
        .find(|t| t.label.to_ascii_lowercase().starts_with("eng"));
    english.or_else(|| tracks.first())
}

// -- provider ---------------------------------------------------------------

pub struct AniBd {
    http: HttpClient,
    api: String,
}

impl AniBd {
    pub fn new() -> Result<AniBd, ProviderError> {
        AniBd::with_endpoint(API.to_string())
    }

    fn with_endpoint(api: String) -> Result<AniBd, ProviderError> {
        Ok(AniBd {
            http: HttpClient::new()?,
            api,
        })
    }

    fn get(&self, url: &str) -> Result<Vec<u8>, ProviderError> {
        self.http.fetch(&Request {
            method: Method::Get,
            url,
            payload: None,
            user_agent: UA,
            extra_headers: &[
                ("Referer", REFERER),
                ("Accept", "application/json, text/html, */*"),
            ],
            accept: Accept::Any2xx,
            deadline: None,
        })
    }

    fn fetch_catalog(&self, anilist_id: &str) -> Result<Vec<CatalogGroup>, ProviderError> {
        let url = format!("{}/api2.php?epid={anilist_id}", self.api);
        let raw = self.get(&url)?;
        parse_catalog(&raw)
    }

    /// Fetch subtitle from a player page and validate it for mpv.
    fn guarded_subtitle(&self, html: &str) -> Option<String> {
        let tracks = parse_subtitles(html);
        let pick = pick_subtitle(&tracks)?;
        let url = &pick.file;
        if is_absolute_url(url) && clean_arg(url) && guard_fetch_url(url).is_ok() {
            Some(url.clone())
        } else {
            None
        }
    }
}

impl StreamProvider for AniBd {
    fn name(&self) -> &'static str {
        "anibd"
    }

    fn display_name(&self) -> &'static str {
        "AniBD"
    }

    /// Tier A: AniList id is the direct API key. First AniList-keyed provider.
    fn canonical_key(&self, show: &Enrichment) -> Option<String> {
        Some(show.anilist_id.to_string())
    }

    fn search(&self, _query: &str, _opts: &SearchOptions) -> Result<Vec<SearchHit>, ProviderError> {
        Err(ProviderError::Unsupported)
    }

    fn supports_search(&self) -> bool {
        false
    }

    /// Track-agnostic episode listing from the catalog.
    fn episodes(
        &self,
        provider_id: &str,
        _tt: Translation,
        _count_hint: Option<u32>,
    ) -> Result<Vec<String>, ProviderError> {
        guard_show_id(provider_id)?;
        let groups = self.fetch_catalog(provider_id)?;
        Ok(episode_labels(&groups))
    }

    fn resolve(
        &self,
        provider_id: &str,
        episode: &str,
        tt: Translation,
        _quality: Quality,
    ) -> Result<StreamLink, ProviderError> {
        guard_show_id(provider_id)?;
        let ep_num: u32 = episode
            .parse()
            .map_err(|_| ProviderError::Decode("invalid episode".into()))?;
        if ep_num == 0 {
            return Err(ProviderError::Decode("invalid episode".into()));
        }

        let groups = self.fetch_catalog(provider_id)?;
        let link = find_link(&groups, ep_num, tt)
            .ok_or_else(|| ProviderError::Decode("no stream for track".into()))?;

        let encoded_link: String = url::form_urlencoded::byte_serialize(link.as_bytes()).collect();
        let players_url = format!("{}/apilink.php?data={encoded_link}", self.api);
        let raw = self.get(&players_url)?;
        let players = parse_players(&raw)?;

        let usable: Vec<_> = players
            .into_iter()
            .filter_map(|p| p.link.filter(|l| !l.is_empty()))
            .collect();
        if usable.is_empty() {
            return Err(ProviderError::Decode("no players".into()));
        }

        for player_url in usable.iter().take(MAX_PLAYER_TRIES) {
            if !is_absolute_url(player_url)
                || !clean_arg(player_url)
                || guard_fetch_url(player_url).is_err()
            {
                continue;
            }
            let origin = url_origin(player_url);
            let referer = format!("{origin}/");
            let html = match self.get(player_url) {
                Ok(body) => String::from_utf8_lossy(&body).into_owned(),
                Err(_) => continue,
            };
            let Some(hls) = extract_video_url(&html, &origin) else {
                continue;
            };
            if !is_absolute_url(&hls) || !clean_arg(&hls) {
                continue;
            }

            let sub_url = if tt == Translation::Sub {
                self.guarded_subtitle(&html)
            } else {
                None
            };

            return Ok(StreamLink {
                url: hls,
                resolution: None,
                referer: Some(referer),
                user_agent: Some(UA.to_string()),
                cloaked_segments: false,
                decloak_segments: false,
                sub_url,
            });
        }

        Err(ProviderError::Decode("no playable source".into()))
    }

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
    fn audio_kind_classifies_by_name() {
        assert_eq!(audio_kind(Some("Sub Server")), "sub");
        assert_eq!(audio_kind(Some("English Dub")), "dub");
        assert_eq!(audio_kind(Some("DUB")), "dub");
        assert_eq!(audio_kind(Some("dubbed")), "dub");
        assert_eq!(audio_kind(None), "sub");
        assert_eq!(audio_kind(Some("")), "sub");
    }

    #[test]
    fn parse_ep_number_from_name_and_slug() {
        let e = EpisodeEntry {
            name: Some("01".into()),
            slug: Some("ep-1".into()),
            link: None,
        };
        assert_eq!(parse_ep_number(&e), Some(1));

        let e = EpisodeEntry {
            name: None,
            slug: Some("3".into()),
            link: None,
        };
        assert_eq!(parse_ep_number(&e), Some(3));

        let e = EpisodeEntry {
            name: Some("0".into()),
            slug: None,
            link: None,
        };
        assert_eq!(parse_ep_number(&e), None);

        let e = EpisodeEntry {
            name: None,
            slug: None,
            link: None,
        };
        assert_eq!(parse_ep_number(&e), None);
    }

    const CATALOG_FIXTURE: &str = r#"[
        {
            "server_name": "Sub Server",
            "server_data": [
                {"name": "01", "slug": "ep-1", "link": "link-sub-1"},
                {"name": "02", "slug": "ep-2", "link": "link-sub-2"},
                {"name": "03", "slug": "ep-3", "link": "link-sub-3"}
            ]
        },
        {
            "server_name": "English Dub",
            "server_data": [
                {"name": "1", "slug": "ep-1", "link": "link-dub-1"},
                {"name": "2", "slug": "ep-2", "link": "link-dub-2"}
            ]
        },
        {
            "server_name": "Empty Group",
            "server_data": []
        }
    ]"#;

    #[test]
    fn parse_catalog_maps_groups_and_drops_empty() {
        let groups = parse_catalog(CATALOG_FIXTURE.as_bytes()).unwrap();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].audio, "sub");
        assert_eq!(groups[0].episodes.len(), 3);
        assert_eq!(groups[0].episodes[0], (1, "link-sub-1".to_string()));
        assert_eq!(groups[1].audio, "dub");
        assert_eq!(groups[1].episodes.len(), 2);
    }

    #[test]
    fn parse_catalog_empty_array_is_ok() {
        let groups = parse_catalog(b"[]").unwrap();
        assert!(groups.is_empty());
    }

    #[test]
    fn episode_labels_union_sorted_deduped() {
        let groups = parse_catalog(CATALOG_FIXTURE.as_bytes()).unwrap();
        let labels = episode_labels(&groups);
        assert_eq!(labels, ["1", "2", "3"]);
    }

    #[test]
    fn find_link_by_track_and_episode() {
        let groups = parse_catalog(CATALOG_FIXTURE.as_bytes()).unwrap();
        assert_eq!(find_link(&groups, 2, Translation::Sub), Some("link-sub-2"));
        assert_eq!(find_link(&groups, 1, Translation::Dub), Some("link-dub-1"));
        assert_eq!(find_link(&groups, 3, Translation::Dub), None);
        assert_eq!(find_link(&groups, 99, Translation::Sub), None);
    }

    #[test]
    fn extract_video_url_absolute() {
        let html = r#"var player = { videoUrl: "https://cdn.example/master.m3u8", other: 1 }"#;
        assert_eq!(
            extract_video_url(html, "https://player.example"),
            Some("https://cdn.example/master.m3u8".to_string())
        );
    }

    #[test]
    fn extract_video_url_relative_slash() {
        let html = r#"videoUrl: "/r2/cache/abc/index.m3u8""#;
        assert_eq!(
            extract_video_url(html, "https://player.example"),
            Some("https://player.example/r2/cache/abc/index.m3u8".to_string())
        );
    }

    #[test]
    fn extract_video_url_relative_bare() {
        let html = r#"videoUrl:"stream/master.m3u8""#;
        assert_eq!(
            extract_video_url(html, "https://player.example"),
            Some("https://player.example/stream/master.m3u8".to_string())
        );
    }

    #[test]
    fn extract_video_url_single_quotes() {
        let html = "videoUrl : 'https://cdn.test/v.m3u8'";
        assert_eq!(
            extract_video_url(html, "https://x"),
            Some("https://cdn.test/v.m3u8".to_string())
        );
    }

    #[test]
    fn extract_video_url_none_when_absent() {
        assert_eq!(
            extract_video_url("<html>no player</html>", "https://x"),
            None
        );
        assert_eq!(extract_video_url(r#"videoUrl: """#, "https://x"), None);
    }

    #[test]
    fn url_origin_extracts_scheme_and_host() {
        assert_eq!(
            url_origin("https://player.example/play2.php?x=1"),
            "https://player.example"
        );
        assert_eq!(
            url_origin("http://cdn.test:8080/path"),
            "http://cdn.test:8080"
        );
        assert_eq!(url_origin("https://bare.host"), "https://bare.host");
    }

    #[test]
    fn parse_subtitles_from_playsub_page() {
        let html = r#"
            var player = new ArtPlayer({
                tracks: [
                    {"label": "English", "file": "https://cdn.test/eng.vtt", "kind": "captions"},
                    {"label": "Spanish", "file": "https://cdn.test/spa.vtt", "kind": "captions"},
                    {"file": "https://cdn.test/thumbs.vtt", "kind": "thumbnails"}
                ],
                videoUrl: "/stream.m3u8"
            });
        "#;
        let tracks = parse_subtitles(html);
        assert_eq!(tracks.len(), 2);
        assert_eq!(tracks[0].label, "English");
        assert_eq!(tracks[0].file, "https://cdn.test/eng.vtt");
        assert_eq!(tracks[1].label, "Spanish");
    }

    #[test]
    fn parse_subtitles_empty_when_no_tracks() {
        assert!(parse_subtitles("<html>no tracks</html>").is_empty());
        assert!(parse_subtitles("tracks: []").is_empty());
    }

    #[test]
    fn parse_subtitles_skips_non_http_files() {
        let html = r#"tracks: [{"label": "X", "file": "/local.vtt", "kind": "captions"}]"#;
        assert!(parse_subtitles(html).is_empty());
    }

    #[test]
    fn pick_subtitle_prefers_english() {
        let tracks = vec![
            SubTrack {
                file: "https://cdn/jp.vtt".into(),
                label: "Japanese".into(),
            },
            SubTrack {
                file: "https://cdn/en.vtt".into(),
                label: "English".into(),
            },
        ];
        assert_eq!(pick_subtitle(&tracks).unwrap().file, "https://cdn/en.vtt");
    }

    #[test]
    fn pick_subtitle_falls_back_to_first() {
        let tracks = vec![SubTrack {
            file: "https://cdn/fr.vtt".into(),
            label: "French".into(),
        }];
        assert_eq!(pick_subtitle(&tracks).unwrap().file, "https://cdn/fr.vtt");
    }

    #[test]
    fn pick_subtitle_none_when_empty() {
        assert!(pick_subtitle(&[]).is_none());
    }

    #[test]
    fn json_string_field_extracts_value() {
        let obj = r#"{"label": "English", "file": "https://cdn/x.vtt"}"#;
        assert_eq!(json_string_field(obj, "label").as_deref(), Some("English"));
        assert_eq!(
            json_string_field(obj, "file").as_deref(),
            Some("https://cdn/x.vtt")
        );
        assert_eq!(json_string_field(obj, "missing"), None);
    }

    #[test]
    fn canonical_key_is_the_anilist_id() {
        let p = AniBd::new().unwrap();
        let show = Enrichment {
            anilist_id: 154587,
            mal_id: Some(52991),
            ..Enrichment::default()
        };
        assert_eq!(p.canonical_key(&show).as_deref(), Some("154587"));
    }

    #[test]
    fn canonical_key_works_without_mal() {
        let p = AniBd::new().unwrap();
        let show = Enrichment {
            anilist_id: 100,
            ..Enrichment::default()
        };
        assert_eq!(p.canonical_key(&show).as_deref(), Some("100"));
    }

    #[test]
    fn episodes_rejects_non_numeric_id() {
        let p = AniBd::new().unwrap();
        assert!(matches!(
            p.episodes("../7", Translation::Sub, None),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn resolve_rejects_zero_episode() {
        let p = AniBd::new().unwrap();
        assert!(matches!(
            p.resolve("154587", "0", Translation::Sub, Quality::Best),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn resolve_rejects_non_numeric_episode() {
        let p = AniBd::new().unwrap();
        assert!(matches!(
            p.resolve("154587", "abc", Translation::Sub, Quality::Best),
            Err(ProviderError::Decode(_))
        ));
    }

    use crate::testutil::{response_with_body, serve_once};

    fn against(response: Vec<u8>) -> AniBd {
        AniBd::with_endpoint(serve_once(response).trim_end_matches('/').to_string()).unwrap()
    }

    #[test]
    fn transport_episodes_from_catalog() {
        let p = against(response_with_body("200 OK", CATALOG_FIXTURE.as_bytes()));
        let eps = p.episodes("154587", Translation::Sub, None).unwrap();
        assert_eq!(eps, ["1", "2", "3"]);
    }

    #[test]
    fn transport_episodes_empty_catalog_is_not_stocked() {
        let p = against(response_with_body("200 OK", b"[]"));
        let eps = p.episodes("154587", Translation::Sub, None).unwrap();
        assert!(eps.is_empty());
    }

    #[test]
    fn transport_episodes_forbidden_maps_to_taxonomy() {
        let p = against(response_with_body("403 Forbidden", b""));
        let got = p.episodes("154587", Translation::Sub, None);
        assert!(matches!(got, Err(ProviderError::Forbidden { status: 403 })));
    }

    #[test]
    fn resolve_skips_private_player_url() {
        // apilink.php returns a loopback player link: the SSRF guard must
        // skip it, not fetch it. With no other candidates, resolve fails.
        let catalog = r#"[{"server_name":"Sub","server_data":[{"name":"1","link":"x"}]}]"#;
        let players = r#"[{"link":"http://169.254.169.254/latest/meta-data/"}]"#;
        // First request: catalog. Second: apilink. The player URL itself
        // must never be fetched, so only two hops need a response.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let cat = response_with_body("200 OK", catalog.as_bytes());
        let pl = response_with_body("200 OK", players.as_bytes());
        std::thread::spawn(move || {
            for resp in [cat, pl] {
                let (mut sock, _) = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = std::io::Read::read(&mut sock, &mut buf);
                let _ = std::io::Write::write_all(&mut sock, &resp);
            }
        });
        let p = AniBd::with_endpoint(format!("http://{addr}")).unwrap();
        let got = p.resolve("154587", "1", Translation::Sub, Quality::Best);
        assert!(matches!(got, Err(ProviderError::Decode(_))));
    }

    #[test]
    fn resolve_encodes_catalog_link_in_url() {
        // A catalog link with query metacharacters must be percent-encoded
        // so it stays inside the `data` param, not injected as siblings.
        let catalog = r#"[{"server_name":"Sub","server_data":[
            {"name":"1","link":"evil&inject=1"}
        ]}]"#;
        // apilink.php response: empty players -> resolve fails, but the
        // point is the link reached the wire encoded, not raw.
        let players = b"[]";
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let cat = response_with_body("200 OK", catalog.as_bytes());
        let pl = response_with_body("200 OK", players);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            // First hop: catalog.
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut sock, &mut buf);
            let _ = std::io::Write::write_all(&mut sock, &cat);
            // Second hop: apilink. Capture the request.
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = std::io::Read::read(&mut sock, &mut buf).unwrap_or(0);
            let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
            let _ = std::io::Write::write_all(&mut sock, &pl);
        });
        let p = AniBd::with_endpoint(format!("http://{addr}")).unwrap();
        let _ = p.resolve("154587", "1", Translation::Sub, Quality::Best);
        let req = rx.recv().unwrap();
        assert!(
            req.contains("data=evil%26inject%3D1"),
            "link must be percent-encoded in the URL, got: {req}"
        );
    }

    #[test]
    fn guarded_subtitle_drops_private_url() {
        let p = AniBd::new().unwrap();
        let html = r#"tracks: [{"label": "English", "file": "http://169.254.169.254/meta", "kind": "captions"}]"#;
        assert_eq!(p.guarded_subtitle(html), None);
    }

    #[test]
    fn guarded_subtitle_accepts_public_url() {
        let p = AniBd::new().unwrap();
        let html = r#"tracks: [{"label": "English", "file": "https://cdn.example/eng.vtt", "kind": "captions"}]"#;
        assert_eq!(
            p.guarded_subtitle(html).as_deref(),
            Some("https://cdn.example/eng.vtt")
        );
    }
}
