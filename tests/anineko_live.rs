//! Live anineko probes (ROD-520). Network-dependent; run explicitly:
//! cargo test --test anineko_live -- --ignored --test-threads=1
//!
//! The provider has no canonical route, so a bind is only ever as good as the
//! title search plus the scorers. `live_search_binds_the_right_frieren` is the
//! discriminating one: three same-franchise entries come back on one query and
//! only the episode count separates them.

use std::thread::sleep;
use std::time::Duration;

use sabigoku::domain::{Enrichment, Quality, Translation};
use sabigoku::providers::anineko::Anineko;
use sabigoku::providers::{ProviderError, SearchOptions, StreamProvider};
use sabigoku::resolver;

const FRIEREN_S1_SLUG: &str = "frieren-beyond-journeys-end";

fn provider() -> Anineko {
    sleep(Duration::from_secs(1));
    Anineko::new().expect("client build")
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
        anilist_id: 154587,
        mal_id: Some(52991),
        title_romaji: "Sousou no Frieren".into(),
        title_english: Some("Frieren: Beyond Journey's End".into()),
        total_episodes: Some(28),
        year: Some(2023),
        status: Some("FINISHED".into()),
        ..Enrichment::default()
    }
}

#[test]
#[ignore = "hits live anineko"]
fn live_search_returns_slug_keyed_hits_with_no_ids() {
    let hits = provider().search("frieren", &opts()).expect("search");
    assert!(!hits.is_empty());
    let s1 = hits
        .iter()
        .find(|h| h.provider_id == FRIEREN_S1_SLUG)
        .expect("frieren s1 in results");
    assert_eq!(s1.total_episodes, Some(28));
    // The payload carries no AniList/MAL id and no year. If either ever starts
    // arriving, tier B becomes available and the binding story changes.
    assert!(hits.iter().all(|h| h.anilist_id.is_none()));
    assert!(hits.iter().all(|h| h.mal_id.is_none()));
    assert!(hits.iter().all(|h| h.year.is_none()));
}

#[test]
#[ignore = "hits live anineko"]
fn live_search_binds_the_right_frieren() {
    // One query returns S1 (28 eps), S2 (10 eps), and the mini-anime ONA. With
    // no ids and no year, the episode count is the entire margin.
    let p = provider();
    let canonical = frieren_s1();
    let hits = p.search(&canonical.title_romaji, &opts()).expect("search");
    let ix = resolver::best_id_match(&canonical, &hits)
        .or_else(|| resolver::best_provider_match(&canonical, &hits))
        .expect("a bind");
    assert_eq!(hits[ix].provider_id, FRIEREN_S1_SLUG);
}

#[test]
#[ignore = "hits live anineko"]
fn live_episodes_lists_a_sorted_grid() {
    let eps = provider()
        .episodes(FRIEREN_S1_SLUG, Translation::Sub, None)
        .expect("episodes");
    assert_eq!(eps.len(), 28);
    assert_eq!(eps.first().map(String::as_str), Some("1"));
    assert_eq!(eps.last().map(String::as_str), Some("28"));
}

#[test]
#[ignore = "hits live anineko"]
fn live_episodes_unknown_slug_errors_rather_than_stamping_absence() {
    // A dead slug 404s; the site has no "known show, zero episodes" page. That
    // must stay an Err: a bound slug that later 404s is a rename or a pull, and
    // Ok(vec![]) would burn it in as a permanent not-stocked verdict (03 §4.3).
    let got = provider().episodes("no-such-show-here", Translation::Sub, None);
    assert!(
        matches!(got, Err(ProviderError::Http { status: 404 })),
        "expected a 404 error, got {got:?}"
    );
}

#[test]
#[ignore = "hits live anineko"]
fn live_resolve_returns_a_cloaked_link_with_a_softsub() {
    let sl = provider()
        .resolve(FRIEREN_S1_SLUG, "1", Translation::Sub, Quality::Best)
        .expect("resolve");
    assert!(sl.url.starts_with("http"));
    assert!(sl.url.contains(".m3u8"));
    // Every segment is PNG-cloaked on the shared ad CDN: without both flags
    // playback is a black screen.
    assert!(sl.cloaked_segments);
    assert!(sl.decloak_segments);
    assert!(sl.sub_url.is_some(), "inline softsub rides the embed url");
}

#[test]
#[ignore = "hits live anineko"]
fn live_resolve_dub_carries_no_softsub() {
    let sl = provider()
        .resolve(FRIEREN_S1_SLUG, "1", Translation::Dub, Quality::Best)
        .expect("resolve");
    assert!(sl.url.starts_with("http"));
    assert_eq!(sl.sub_url, None);
}

#[test]
#[ignore = "hits live anineko"]
fn live_resolve_honors_a_quality_cap() {
    let sl = provider()
        .resolve(FRIEREN_S1_SLUG, "1", Translation::Sub, Quality::P480)
        .expect("resolve");
    // The ladder is 360/720/1080, so a 480 cap takes the highest at or below it.
    assert!(
        sl.url.contains("360"),
        "expected the 360p rung, got {}",
        sl.url
    );
}
