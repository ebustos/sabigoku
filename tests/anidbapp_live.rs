//! Live anidb.app probes (ROD-516). Network-dependent; run explicitly:
//! cargo test --test anidbapp_live -- --ignored --test-threads=1
//! The search chain scrapes HTML, so a red run here usually means the card or
//! detail markup moved, not that the seam broke.

use std::thread::sleep;
use std::time::Duration;

use sabigoku::domain::{Enrichment, Quality, Translation};
use sabigoku::providers::anidbapp::AniDbApp;
use sabigoku::providers::{SearchOptions, StreamProvider};
use sabigoku::resolver;

/// Frieren S1: site id 1663, AniList 154587. Its S2 entry is a separate site
/// id that a title match alone cannot separate, which is the point of the
/// id-confirm.
const FRIEREN_S1_SITE: &str = "1663";
const FRIEREN_S1_ANILIST: i64 = 154587;
const FRIEREN_S1_MAL: i64 = 52991;
/// Frieren S2: the site numbers it 29..38 while its AniList entry starts at 1.
const FRIEREN_S2_SITE: &str = "1665";

fn provider() -> AniDbApp {
    sleep(Duration::from_secs(1));
    AniDbApp::new().expect("client build")
}

fn opts() -> SearchOptions {
    SearchOptions {
        translation: Translation::Sub,
        limit: 26,
        page: 1,
    }
}

fn frieren_s1() -> Enrichment {
    Enrichment {
        anilist_id: FRIEREN_S1_ANILIST,
        mal_id: Some(FRIEREN_S1_MAL),
        title_romaji: "Sousou no Frieren".into(),
        year: Some(2023),
        ..Enrichment::default()
    }
}

#[test]
#[ignore = "hits live anidb.app"]
fn live_search_carries_the_external_ids() {
    let hits = provider().search("frieren", &opts()).expect("search");
    assert!(!hits.is_empty(), "no cards parsed");
    assert!(
        hits.iter().any(|h| h.anilist_id.is_some()),
        "no candidate carried an AniList id: the detail-page scrape broke"
    );
    let s1 = hits
        .iter()
        .find(|h| h.provider_id == FRIEREN_S1_SITE)
        .expect("frieren s1 among the cards");
    assert_eq!(s1.anilist_id, Some(FRIEREN_S1_ANILIST));
    assert_eq!(s1.mal_id, Some(FRIEREN_S1_MAL));
}

/// The whole binding story: several same-franchise cards come back, and the id
/// confirm picks the right one. A title match alone cannot do this.
#[test]
#[ignore = "hits live anidb.app"]
fn live_search_binds_the_right_season() {
    let hits = provider().search("frieren", &opts()).expect("search");
    let ix = resolver::best_id_match(&frieren_s1(), &hits).expect("id match");
    assert_eq!(hits[ix].provider_id, FRIEREN_S1_SITE);
}

#[test]
#[ignore = "hits live anidb.app"]
fn live_episodes_lists_a_sorted_grid() {
    let eps = provider()
        .episodes(FRIEREN_S1_SITE, Translation::Sub, None)
        .expect("episodes");
    assert_eq!(eps.len(), 28);
    assert_eq!(eps.first().map(String::as_str), Some("1"));
    assert_eq!(eps.last().map(String::as_str), Some("28"));
}

/// The site numbers this season 29..38; the grid must read 1..10 or every
/// label is off by a season.
#[test]
#[ignore = "hits live anidb.app"]
fn live_episodes_normalize_absolute_season_numbering() {
    let eps = provider()
        .episodes(FRIEREN_S2_SITE, Translation::Sub, None)
        .expect("episodes");
    assert_eq!(eps.first().map(String::as_str), Some("1"));
}

#[test]
#[ignore = "hits live anidb.app"]
fn live_dub_listing_is_bounded() {
    let eps = provider()
        .episodes(FRIEREN_S1_SITE, Translation::Dub, None)
        .expect("episodes");
    assert!(!eps.is_empty(), "frieren s1 is dubbed");
    assert!(eps.len() <= 28);
}

#[test]
#[ignore = "hits live anidb.app"]
fn live_resolve_returns_a_playable_link() {
    let sl = provider()
        .resolve(FRIEREN_S1_SITE, "1", Translation::Sub, Quality::Best)
        .expect("resolve");
    assert!(sl.url.contains(".m3u8"), "not an HLS master: {}", sl.url);
    // Segments are TS renamed .xls, with no decoy prefix: mpv relaxes its
    // demuxer gate and the stripping proxy stays out.
    assert!(sl.cloaked_segments);
    assert!(!sl.decloak_segments);
}

/// A cap has to actually select a variant. `cap_variant` returns None on any
/// failure and resolve then keeps the master, so "an m3u8 came back" would pass
/// just as happily on a silent no-op.
#[test]
#[ignore = "hits live anidb.app"]
fn live_resolve_honors_a_quality_cap() {
    let capped = provider()
        .resolve(FRIEREN_S1_SITE, "1", Translation::Sub, Quality::P720)
        .expect("resolve");
    assert!(capped.url.contains(".m3u8"));
    assert!(
        !capped.url.ends_with("master.m3u8"),
        "cap fell back to the master ladder: {}",
        capped.url
    );
}

/// The offset has to survive resolve too, not just the listing.
#[test]
#[ignore = "hits live anidb.app"]
fn live_resolve_maps_an_offset_season() {
    let sl = provider()
        .resolve(FRIEREN_S2_SITE, "1", Translation::Sub, Quality::Best)
        .expect("resolve");
    assert!(sl.url.contains(".m3u8"));
}
