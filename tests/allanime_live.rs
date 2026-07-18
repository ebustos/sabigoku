//! Live AllAnime probes (ROD-436). Network-dependent; run explicitly:
//! cargo test --test allanime_live -- --ignored --test-threads=1
//! The site rotates persisted-query hashes without notice, so a red run here
//! means "check the hashes", not "the port broke".

use std::thread::sleep;
use std::time::Duration;

use sabigoku::domain::Quality;
use sabigoku::domain::Translation;
use sabigoku::providers::allanime::AllAnime;
use sabigoku::providers::{SearchOptions, StreamProvider};

const FRIEREN_S1_ID: &str = "ReHMC7TQnch3C6z8j";

fn provider() -> AllAnime {
    sleep(Duration::from_secs(2));
    AllAnime::new().expect("client build")
}

fn opts() -> SearchOptions {
    SearchOptions {
        translation: Translation::Sub,
        limit: 26,
        page: 1,
    }
}

#[test]
#[ignore = "hits live AllAnime"]
fn live_search_finds_frieren_and_mines_its_id() {
    let hits = provider().search("frieren", &opts()).expect("search");
    assert!(!hits.is_empty());
    // The exact-title hit should mine AniList id 154587 off its cover thumb.
    let frieren = hits
        .iter()
        .find(|h| h.provider_id == FRIEREN_S1_ID)
        .expect("frieren s1 in results");
    assert_eq!(frieren.anilist_id, Some(154587));
}

#[test]
#[ignore = "hits live AllAnime"]
fn live_episodes_lists_a_sorted_grid() {
    let eps = provider()
        .episodes(FRIEREN_S1_ID, Translation::Sub, None)
        .expect("episodes");
    // 28-episode season; the contract is ascending, so "1" leads.
    assert!(eps.len() >= 28);
    assert_eq!(eps.first().map(String::as_str), Some("1"));
}

#[test]
#[ignore = "hits live AllAnime"]
fn live_resolve_returns_a_playable_link() {
    let sl = provider()
        .resolve(FRIEREN_S1_ID, "1", Translation::Sub, Quality::Best)
        .expect("resolve");
    assert!(sl.url.starts_with("http"));
}
