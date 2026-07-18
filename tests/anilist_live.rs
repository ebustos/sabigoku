//! Live AniList probes (ROD-435 exit criterion). Network-dependent; run
//! explicitly: cargo test --test anilist_live -- --ignored --test-threads=1
//! (serial + one endpoint pause per test keeps us polite to the API).

use std::thread::sleep;
use std::time::Duration;

use sabigoku::anilist::AniList;
use sabigoku::providers::{CatalogError, CatalogProvider, DISCOVER_PAGE_SIZE, DiscoverAxis};

fn client() -> AniList {
    sleep(Duration::from_secs(2));
    AniList::new().expect("client build")
}

#[test]
#[ignore = "hits live AniList"]
fn live_search_lands_a_full_page() {
    let page = client().search("frieren", 1).expect("search");
    assert!(!page.entries.is_empty());
    assert!(page.entries.iter().any(|e| e.anilist_id == 154587));
    assert!(page.entries.iter().all(|e| e.anilist_id > 0));
}

#[test]
#[ignore = "hits live AniList"]
fn live_discover_trending_fills_a_page() {
    let page = client()
        .discover(DiscoverAxis::Trending, 1)
        .expect("discover");
    assert_eq!(page.entries.len(), DISCOVER_PAGE_SIZE as usize);
    assert!(page.has_next);
}

#[test]
#[ignore = "hits live AniList"]
fn live_discover_this_season_binds_the_cour() {
    let page = client()
        .discover(DiscoverAxis::ThisSeason, 1)
        .expect("discover");
    assert!(!page.entries.is_empty());
}

#[test]
#[ignore = "hits live AniList"]
fn live_enrich_by_id_returns_metadata() {
    let e = client().enrich(154587).expect("enrich").expect("metadata");
    assert_eq!(e.title_romaji, "Sousou no Frieren");
    assert_eq!(e.mal_id, Some(52991));
}

#[test]
#[ignore = "hits live AniList"]
fn live_enrich_unknown_id_is_http_404_not_confirmed_null() {
    // AniList answers a nonexistent id with HTTP 404 (data.Media null rides
    // along). Under the freeze rules that is no-answer, not confirmed-null;
    // this pins the live reality the classify tests assume.
    match client().enrich(999_999_999) {
        Err(CatalogError::Http { status: 404 }) => {}
        other => panic!("expected Http 404, got {other:?}"),
    }
}
