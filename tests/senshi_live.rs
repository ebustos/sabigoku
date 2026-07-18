//! Live senshi probes (ROD-441). Network-dependent; run explicitly:
//! cargo test --test senshi_live -- --ignored --test-threads=1
//! Ported from zigoku v0.4.7; senshi rotates its filter body without notice,
//! so a red run here means check the query shape, not the port.

use std::thread::sleep;
use std::time::Duration;

use sabigoku::domain::{Quality, Translation};
use sabigoku::providers::senshi::Senshi;
use sabigoku::providers::{SearchOptions, StreamProvider};

const FRIEREN_S1_MAL: &str = "52991";

fn provider() -> Senshi {
    sleep(Duration::from_secs(1));
    Senshi::new().expect("client build")
}

fn opts() -> SearchOptions {
    SearchOptions {
        translation: Translation::Sub,
        limit: 26,
        page: 1,
    }
}

#[test]
#[ignore = "hits live senshi"]
fn live_search_finds_frieren_by_mal() {
    let hits = provider().search("frieren", &opts()).expect("search");
    assert!(!hits.is_empty());
    let s1 = hits
        .iter()
        .find(|h| h.provider_id == FRIEREN_S1_MAL)
        .expect("frieren s1 in results");
    assert_eq!(s1.mal_id, Some(52991));
}

#[test]
#[ignore = "hits live senshi"]
fn live_episodes_lists_a_sorted_grid() {
    let eps = provider()
        .episodes(FRIEREN_S1_MAL, Translation::Sub, None)
        .expect("episodes");
    assert!(eps.len() >= 28);
    assert_eq!(eps.first().map(String::as_str), Some("1"));
}

#[test]
#[ignore = "hits live senshi"]
fn live_resolve_returns_a_playable_link() {
    let sl = provider()
        .resolve(FRIEREN_S1_MAL, "1", Translation::Sub, Quality::Best)
        .expect("resolve");
    assert!(sl.url.starts_with("http"));
    assert!(sl.cloaked_segments);
}
