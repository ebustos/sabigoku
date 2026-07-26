//! Live update-check probes (ROD-466). Network-dependent; run explicitly:
//! cargo test --test update_live -- --ignored --test-threads=1

use sabigoku::updatecheck;

/// The `sabigoku update` freshness path: bypasses the TTL cache, answers with
/// the real latest tag, refreshes the cache to a well-formed two-line body.
#[test]
#[ignore]
fn latest_fresh_answers_live_and_refreshes_the_cache() {
    let dir = std::env::temp_dir().join("sabigoku-update-live");
    let _ = std::fs::remove_dir_all(&dir);
    let now = 1_700_000_000;

    let tag = updatecheck::latest_fresh(&dir, now).expect("GitHub reachable");
    assert!(
        tag.starts_with('v') && tag[1..].split('.').count() == 3,
        "latest tag looks like a release: {tag}"
    );

    let cached = std::fs::read_to_string(dir.join("update_check")).expect("cache written");
    assert_eq!(cached, format!("{now}\n{tag}\n"));

    // Stale cache seeded with a fake tag: latest_fresh must overwrite it with
    // the network answer, proving it never serves the cache.
    std::fs::write(dir.join("update_check"), format!("{now}\nv999.0.0\n")).unwrap();
    let again = updatecheck::latest_fresh(&dir, now + 1).expect("GitHub reachable");
    assert_eq!(again, tag, "cache bypassed, network answered");
}
