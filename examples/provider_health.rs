//! provider_health (ROD-521): grade every provider in `default_registry()`
//! end to end and report where each one breaks.
//!
//! Hits the live sites. Not a test, not in CI: the output is the deliverable
//! and a provider dying must never redden master.
//!
//! Run:  cargo run --example provider_health
//!       cargo run --example provider_health -- --provider senshi --dub
//!       SABIGOKU_DEBUG=1 cargo run --example provider_health
//!
//! Exit: 0 all healthy, 1 something degraded, 2 something down.
//!
//! Every grade here is allowed to say "I could not confirm this", and several
//! do. A tool that guesses OK is worse than no tool: it is the thing that let
//! a provider die unnoticed in the first place.
//!
//! Worst-case wall clock is roughly 10 minutes (5 providers, 3 fixtures, 4
//! network calls each, all blackholing to the transport's 10s deadline). A
//! healthy full run is about 30 seconds. There is no concurrency on purpose;
//! parallel probes against the same host change what the host does.
//!
//! `--dub` grades loosely, and how loosely depends on the provider. Where the
//! episode listing is track-aware (allanime, anidbapp) a missing dub is an
//! authoritative empty listing and reads NOT-STOCKED. Where the listing
//! ignores translation (megaplay, senshi, anibd) the absence only surfaces at
//! resolve and reads DEGRADED. Separating the two would mean matching on
//! provider error text, which breaks the moment a site rewords.

use std::fmt;
use std::process::ExitCode;
use std::time::Instant;

use sabigoku::domain::{Enrichment, Quality, StreamLink, Translation, strip_controls};
use sabigoku::fetchguard::guard_fetch_url;
use sabigoku::providers::http::{Accept, HttpClient, Method, Request};
use sabigoku::providers::{
    ProviderError, SearchHit, SearchOptions, StreamProvider, default_registry,
};

/// mpv's own UA (player.rs), so a reach probe that passes means the player's
/// fetch would too. A provider-supplied UA on the link still wins.
const PLAYER_UA: &str = "Mozilla/5.0 (X11; Linux) Gecko";

/// First bytes only; enough to prove the CDN serves us, small enough that a
/// full run costs nothing.
const REACH_RANGE: &str = "bytes=0-65535";

struct Fixture {
    anilist_id: i64,
    mal_id: i64,
    title: &'static str,
    /// `count_hint` for listing-less providers (03 §4.3).
    episodes: u32,
}

/// Three eras, all universally stocked. Plural on purpose: one fixture makes a
/// delisting look like a provider death (ROD-521).
const FIXTURES: &[Fixture] = &[
    Fixture {
        anilist_id: 154587,
        mal_id: 52991,
        title: "Sousou no Frieren",
        episodes: 28,
    },
    Fixture {
        anilist_id: 1,
        mal_id: 1,
        title: "Cowboy Bebop",
        episodes: 26,
    },
    Fixture {
        anilist_id: 21,
        mal_id: 21,
        title: "One Piece",
        episodes: 1100,
    },
];

impl Fixture {
    fn enrichment(&self) -> Enrichment {
        Enrichment {
            anilist_id: self.anilist_id,
            mal_id: Some(self.mal_id),
            title_romaji: self.title.to_string(),
            total_episodes: Some(self.episodes),
            ..Enrichment::default()
        }
    }
}

/// Declaration order is the per-fixture preference, and `min` across fixtures
/// picks a provider's grade: one fixture reaching Ok proves the provider works,
/// whatever the others did. Degraded beats NotStocked here because it carries
/// more signal (bind and list both answered), not because it is milder.
///
/// Do NOT reuse this order to roll providers up into one headline. That
/// question is "what is the worst thing on the board", which ranks the
/// variants differently; `severity` owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    Ok,
    Degraded,
    NotStocked,
    Skip,
    Down,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Ok => "OK",
            Verdict::Degraded => "DEGRADED",
            Verdict::NotStocked => "NOT-STOCKED",
            Verdict::Skip => "SKIP",
            Verdict::Down => "DOWN",
        }
    }

    /// How alarming, for the cross-provider rollup.
    fn severity(self) -> u8 {
        match self {
            Verdict::Ok => 0,
            Verdict::NotStocked => 1,
            Verdict::Skip => 2,
            Verdict::Degraded => 3,
            Verdict::Down => 4,
        }
    }

    fn exit(self) -> ExitCode {
        match self {
            Verdict::Ok => ExitCode::SUCCESS,
            Verdict::Down => ExitCode::from(2),
            _ => ExitCode::from(1),
        }
    }
}

/// One provider against one fixture: the grade plus the stage trail that
/// produced it.
struct Probe {
    verdict: Verdict,
    stages: Vec<String>,
    detail: Option<String>,
}

impl Probe {
    fn graded(verdict: Verdict, stages: Vec<String>, detail: Option<String>) -> Probe {
        Probe {
            verdict,
            stages,
            detail,
        }
    }
}

impl fmt::Display for Probe {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:<12}", self.verdict.label())?;
        write!(f, "{}", self.stages.join("  "))?;
        if let Some(detail) = &self.detail {
            write!(f, "  {detail}")?;
        }
        Ok(())
    }
}

fn secs(at: Instant) -> String {
    format!("{:.2}s", at.elapsed().as_secs_f64())
}

/// Provider-controlled text (ids, episode labels, decode errors quoting a
/// response body) reaches a terminal here. Same reasoning as `log_url` in
/// providers/http.rs: a bare CR or an ANSI escape in a search-hit id can
/// overwrite the line that would have shown a real failure, forging the report
/// a human reads. Truncated because an id is not a payload.
fn safe(s: &str) -> String {
    let mut out = strip_controls(s.to_string());
    if out.chars().count() > 80 {
        out = out.chars().take(80).collect::<String>() + "...";
    }
    out
}

/// Prefer a hit the provider itself id-keyed to the fixture; fall back to the
/// site's top result. The bool is whether the bind was id-verified, and it
/// caps the whole probe: a search backend that degrades to answering every
/// query with its top result would otherwise resolve, reach, and grade OK on
/// an unrelated show, which is precisely the death this tool exists to catch.
fn pick_hit<'a>(hits: &'a [SearchHit], fx: &Fixture) -> (&'a SearchHit, bool) {
    let keyed = hits
        .iter()
        .find(|h| h.anilist_id == Some(fx.anilist_id) || h.mal_id == Some(fx.mal_id));
    match keyed {
        Some(hit) => (hit, true),
        None => (&hits[0], false),
    }
}

/// Ranged GET with the link's own headers.
///
/// A 3xx is NOT graded reachable. The transport bans redirects so a provider
/// URL cannot escape the fetchguard (03 §6.7), which leaves the destination
/// unknown: mpv would follow it, and it is just as likely to be a working CDN
/// as a bounce to an interstitial. Reporting "unverified" is the only truthful
/// grade available without a header-returning seam in providers/http.rs.
fn reach(http: &HttpClient, link: &StreamLink) -> Result<String, String> {
    guard_fetch_url(&link.url).map_err(|e| format!("guard: {e}"))?;
    let ua = link.user_agent.as_deref().unwrap_or(PLAYER_UA);
    let mut headers: Vec<(&str, &str)> = vec![("Range", REACH_RANGE)];
    if let Some(referer) = &link.referer {
        headers.push(("Referer", referer));
    }
    let req = Request {
        method: Method::Get,
        url: &link.url,
        payload: None,
        user_agent: ua,
        extra_headers: &headers,
        accept: Accept::Any2xx,
        deadline: None,
    };
    match http.fetch(&req) {
        Ok(body) => sniff(&body),
        Err(ProviderError::Http { status }) if (300..400).contains(&status) => Err(format!(
            "{status} redirect, destination unverified (transport cannot follow)"
        )),
        Err(e) => Err(safe(&e.to_string())),
    }
}

/// Status alone is not reach: an anti-bot wall serves its challenge as a 200,
/// so anything that merely arrived would grade a corpse healthy.
///
/// Allowlist, not blocklist. Every provider resolves to an HLS playlist today,
/// so a playlist carrying at least one real reference is the only shape this
/// can affirm. Everything else is reported with its size and left ungraded
/// rather than guessed at, which is also how a future non-HLS provider will
/// announce itself instead of silently reading as broken.
fn sniff(body: &[u8]) -> Result<String, String> {
    if body.is_empty() {
        return Err("empty body (2xx with no bytes)".into());
    }
    let head = String::from_utf8_lossy(&body[..body.len().min(4096)]);
    // A BOM is not whitespace, so trim_start alone would let a byte-order-mark
    // in front of a challenge page walk past every check below.
    let text = head.trim_start_matches('\u{feff}').trim_start();
    if !text.starts_with("#EXTM3U") {
        return Err(format!(
            "unrecognized body, {}B (not a playlist)",
            body.len()
        ));
    }
    // #EXTM3U with nothing under it is what a stub or a truncated error page
    // looks like; a real playlist names a variant or a segment.
    let refs = text
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .count();
    if refs == 0 {
        return Err(format!("playlist with no references, {}B", body.len()));
    }
    Ok(format!("m3u8 {refs} refs {}B", body.len()))
}

fn probe(provider: &dyn StreamProvider, fx: &Fixture, tt: Translation, http: &HttpClient) -> Probe {
    let mut stages: Vec<String> = Vec::new();
    let show = fx.enrichment();

    let at = Instant::now();
    let (id, verified) = match provider.canonical_key(&show) {
        Some(key) => {
            stages.push(format!("bind:canonical({}) {}", safe(&key), secs(at)));
            (key, true)
        }
        None => {
            if !provider.supports_search() {
                return Probe::graded(
                    Verdict::Skip,
                    stages,
                    Some("unbindable: no canonical key and no search".into()),
                );
            }
            let opts = SearchOptions {
                translation: tt,
                limit: 26,
                page: 1,
            };
            match provider.search(fx.title, &opts) {
                Err(e) => {
                    stages.push(format!("bind:search {}", secs(at)));
                    return Probe::graded(
                        Verdict::Down,
                        stages,
                        Some(format!("search: {}", safe(&e.to_string()))),
                    );
                }
                Ok(hits) if hits.is_empty() => {
                    stages.push(format!("bind:search 0 hits {}", secs(at)));
                    return Probe::graded(Verdict::NotStocked, stages, None);
                }
                Ok(hits) => {
                    let (hit, keyed) = pick_hit(&hits, fx);
                    let how = if keyed { "keyed" } else { "top-hit" };
                    stages.push(format!(
                        "bind:search {how}({}) {}",
                        safe(&hit.provider_id),
                        secs(at)
                    ));
                    (hit.provider_id.clone(), keyed)
                }
            }
        }
    };

    // Ok(vec![]) is authoritative absence, never a failure (03 §4.3).
    let at = Instant::now();
    let labels = match provider.episodes(&id, tt, Some(fx.episodes)) {
        Err(e) => {
            stages.push(format!("list {}", secs(at)));
            return Probe::graded(
                Verdict::Down,
                stages,
                Some(format!("episodes: {}", safe(&e.to_string()))),
            );
        }
        Ok(labels) if labels.is_empty() => {
            stages.push(format!("list:none {}", secs(at)));
            return Probe::graded(Verdict::NotStocked, stages, None);
        }
        Ok(labels) => {
            stages.push(format!("list:{} {}", labels.len(), secs(at)));
            labels
        }
    };

    let at = Instant::now();
    let link = match provider.resolve(&id, &labels[0], tt, Quality::Best) {
        Err(e) => {
            stages.push(format!("resolve {}", secs(at)));
            return Probe::graded(
                Verdict::Degraded,
                stages,
                Some(format!("resolve: {}", safe(&e.to_string()))),
            );
        }
        Ok(link) => {
            stages.push(format!("resolve:ep{} {}", safe(&labels[0]), secs(at)));
            link
        }
    };

    // The stage a contract test cannot cover: resolve can hand back a
    // well-formed URL that the CDN refuses to serve.
    let at = Instant::now();
    match reach(http, &link) {
        Err(e) => {
            stages.push(format!("reach {}", secs(at)));
            Probe::graded(Verdict::Degraded, stages, Some(format!("reach: {e}")))
        }
        Ok(note) if !verified => {
            stages.push(format!("reach:{note} {}", secs(at)));
            Probe::graded(
                Verdict::Degraded,
                stages,
                Some("unverified bind: search never id-keyed this hit, the stream may be another show".into()),
            )
        }
        Ok(note) => {
            stages.push(format!("reach:{note} {}", secs(at)));
            Probe::graded(Verdict::Ok, stages, None)
        }
    }
}

/// A provider's grade is its best fixture: one success proves the provider
/// works and the rest were stocking gaps.
///
/// The exception is a clean sweep of absence. The fixtures are chosen to be
/// universally stocked, so all of them missing is not three delistings, it is
/// a catalog or search endpoint answering "nothing" to everything, which reads
/// far too calm as NOT-STOCKED.
fn grade(probes: Vec<Probe>) -> (Verdict, Option<String>) {
    let swept = probes.iter().all(|p| p.verdict == Verdict::NotStocked);
    if swept {
        return (
            Verdict::Down,
            Some(format!(
                "every fixture absent ({} of {}); the catalog answers nothing, not a delisting",
                probes.len(),
                FIXTURES.len()
            )),
        );
    }
    let best = probes
        .into_iter()
        .min_by_key(|p| p.verdict)
        .expect("FIXTURES is never empty");
    (best.verdict, best.detail)
}

struct Args {
    provider: Option<String>,
    translation: Translation,
}

fn parse_args() -> Result<Args, String> {
    let mut provider = None;
    let mut translation = Translation::Sub;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--provider" => provider = Some(args.next().ok_or("--provider needs a value")?),
            "--dub" => translation = Translation::Dub,
            other => {
                return Err(format!(
                    "unknown argument {other:?}\nusage: provider_health [--provider NAME] [--dub]"
                ));
            }
        }
    }
    Ok(Args {
        provider,
        translation,
    })
}

fn run() -> Result<Verdict, String> {
    let args = parse_args()?;
    let registry = default_registry().map_err(|e| format!("registry: {e}"))?;
    let http = HttpClient::new().map_err(|e| format!("http client: {e}"))?;

    let providers: Vec<&dyn StreamProvider> = match &args.provider {
        Some(name) => vec![
            registry
                .by_name(name)
                .ok_or_else(|| format!("unknown provider {name:?}"))?,
        ],
        None => registry.iter().collect(),
    };

    let tt = match args.translation {
        Translation::Sub => "sub",
        Translation::Dub => "dub",
    };
    println!("probing {} provider(s), {tt}\n", providers.len());

    let mut summary: Vec<(&'static str, Verdict, Option<String>)> = Vec::new();
    for provider in providers {
        println!("== {} ==", provider.name());
        let mut probes = Vec::with_capacity(FIXTURES.len());
        for fx in FIXTURES {
            let probe = probe(provider, fx, args.translation, &http);
            println!("  {:<20} {probe}", fx.title);
            probes.push(probe);
        }
        let (verdict, detail) = grade(probes);
        println!("  verdict: {}\n", verdict.label());
        summary.push((provider.name(), verdict, detail));
    }

    println!("== summary ==");
    let mut overall = Verdict::Ok;
    for (name, verdict, detail) in &summary {
        let detail = detail.as_deref().unwrap_or("");
        println!("  {name:<12} {:<12} {detail}", verdict.label());
        if verdict.severity() > overall.severity() {
            overall = *verdict;
        }
    }
    Ok(overall)
}

fn main() -> ExitCode {
    // Opt-in only. The provider log sites (ROD-483) are no-ops until a logger
    // is installed, so without this SABIGOKU_DEBUG does nothing; installing it
    // unconditionally is worse, because the always-on transport warns
    // interleave into the table and shred the report they are meant to explain.
    if sabigoku::logging::env_debug() {
        sabigoku::logging::init_stderr(true);
    }
    match run() {
        Ok(verdict) => {
            println!("\nworst: {}", verdict.label());
            verdict.exit()
        }
        Err(e) => {
            eprintln!("provider_health: {e}");
            ExitCode::from(2)
        }
    }
}

/// Not reached by `cargo test` (examples need `--all-targets`), but these are
/// the pure deciders behind every grade and both ROD-521 reviewers landed real
/// bugs in them, so they get a net.
#[cfg(test)]
mod tests {
    use super::*;

    fn hit(provider_id: &str, anilist: Option<i64>, mal: Option<i64>) -> SearchHit {
        SearchHit {
            provider_id: provider_id.into(),
            anilist_id: anilist,
            mal_id: mal,
            ..SearchHit::default()
        }
    }

    #[test]
    fn sniff_rejects_an_empty_two_hundred() {
        assert!(sniff(b"").is_err());
    }

    #[test]
    fn sniff_rejects_a_challenge_page_hiding_behind_a_bom() {
        // trim_start does not eat U+FEFF, so the BOM was a free bypass.
        let body = "\u{feff}<!DOCTYPE html><html>captcha</html>".as_bytes();
        assert!(sniff(body).is_err());
    }

    #[test]
    fn sniff_rejects_a_json_block_page() {
        assert!(sniff(br#"{"blocked":true,"reason":"bot"}"#).is_err());
    }

    #[test]
    fn sniff_rejects_a_playlist_header_with_no_references() {
        // Magic bytes are cheap to forge; a stub carries no variant or segment.
        assert!(sniff(b"#EXTM3U\n#EXT-X-VERSION:3\n").is_err());
    }

    #[test]
    fn sniff_accepts_a_playlist_that_names_a_variant() {
        let body = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=800000\nindex.m3u8\n";
        assert!(sniff(body).is_ok());
    }

    #[test]
    fn pick_hit_prefers_an_id_keyed_hit_over_the_top_result() {
        let fx = &FIXTURES[0];
        let hits = vec![
            hit("wrong-show", None, None),
            hit("right-show", Some(fx.anilist_id), None),
        ];
        let (picked, keyed) = pick_hit(&hits, fx);
        assert_eq!(picked.provider_id, "right-show");
        assert!(keyed);
    }

    #[test]
    fn pick_hit_flags_the_top_hit_fallback_as_unverified() {
        // The flag is what stops a search that answers everything with its
        // top result from grading OK on an unrelated show.
        let hits = vec![hit("whatever-was-first", None, None)];
        let (picked, keyed) = pick_hit(&hits, &FIXTURES[0]);
        assert_eq!(picked.provider_id, "whatever-was-first");
        assert!(!keyed);
    }

    #[test]
    fn a_clean_sweep_of_absence_is_a_dead_catalog_not_three_delistings() {
        let probes = FIXTURES
            .iter()
            .map(|_| Probe::graded(Verdict::NotStocked, vec![], None))
            .collect();
        let (verdict, detail) = grade(probes);
        assert_eq!(verdict, Verdict::Down);
        assert!(detail.is_some());
    }

    #[test]
    fn one_stocked_fixture_carries_the_provider() {
        let probes = vec![
            Probe::graded(Verdict::NotStocked, vec![], None),
            Probe::graded(Verdict::Ok, vec![], None),
            Probe::graded(Verdict::NotStocked, vec![], None),
        ];
        assert_eq!(grade(probes).0, Verdict::Ok);
    }

    #[test]
    fn rollup_ranks_a_real_failure_above_mere_absence() {
        // The per-fixture Ord says the opposite on purpose; reusing it here
        // would print "worst: NOT-STOCKED" while a provider was degraded.
        assert!(Verdict::Degraded.severity() > Verdict::NotStocked.severity());
        assert!(Verdict::Down.severity() > Verdict::Degraded.severity());
        assert!(Verdict::NotStocked.severity() > Verdict::Ok.severity());
    }

    #[test]
    fn safe_strips_an_id_that_would_forge_a_report_row() {
        let forged = "1\x1b[2K\rmegaplay     OK";
        let out = safe(forged);
        assert!(!out.contains('\r'));
        assert!(!out.contains('\x1b'));
    }

    #[test]
    fn safe_truncates_an_oversize_id() {
        assert!(safe(&"a".repeat(500)).chars().count() <= 83);
    }
}
