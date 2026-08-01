//! provider_health (ROD-521): grade every provider in `default_registry()`
//! end to end and report where each one breaks. `--provider` also reaches the
//! shelf (`retired_registry`), which is how a retirement gets re-checked.
//!
//! Hits the live sites. The probe itself is not a test and never runs in CI:
//! the output is the deliverable and a provider dying must never redden master.
//! CI does compile and run the pure unit tests below, via `--all-targets`;
//! `main` is not the entry point under a test harness, so nothing here reaches
//! the network. Keep it that way: a live `#[test]` in this module would put
//! every provider site on the critical path of every push.
//!
//! Run:  cargo run --example provider_health
//!       cargo run --example provider_health -- --provider senshi --dub
//!       cargo run --example provider_health -- --provider allanime
//!       SABIGOKU_DEBUG=1 cargo run --example provider_health
//!
//! Exit: 0 all healthy, 1 something degraded, 2 something down.
//!
//! Sequential on purpose: parallel probes against one host change what the
//! host does. ~30s healthy, ~10min if everything blackholes.
//!
//! Under `--dub` a missing dub reads NOT-STOCKED on track-aware listings
//! (allanime, anidbapp) and DEGRADED on the rest, which only find out at
//! resolve. Telling those apart needs provider error text; not worth it.

use std::fmt;
use std::process::ExitCode;
use std::time::Instant;

use sabigoku::domain::{Enrichment, Quality, StreamLink, Translation, strip_controls};
use sabigoku::fetchguard::guard_fetch_url;
use sabigoku::providers::http::{Accept, HttpClient, Method, Request};
use sabigoku::providers::{
    ProviderError, SearchHit, SearchOptions, StreamProvider, default_registry, retired_registry,
};
use sabigoku::resolver;

/// mpv's own UA (player.rs): a reach that passes here would pass in playback.
const PLAYER_UA: &str = "Mozilla/5.0 (X11; Linux) Gecko";

/// First bytes only; a full run should cost nothing.
const REACH_RANGE: &str = "bytes=0-65535";

struct Fixture {
    anilist_id: i64,
    mal_id: i64,
    title: &'static str,
    /// Verbatim AniList `title.english`. The scorer reads every title form.
    title_english: &'static str,
    /// `count_hint` for listing-less providers (03 §4.3).
    episodes: u32,
    /// Verbatim AniList `status`. Unset reads as authoritative, which
    /// hard-rejects any count off by more than 3 and makes the probe stricter
    /// than the app.
    status: &'static str,
    year: u32,
}

/// Three eras, all universally stocked. Plural on purpose: with one fixture, a
/// delisting reads as a provider death.
const FIXTURES: &[Fixture] = &[
    Fixture {
        anilist_id: 154587,
        mal_id: 52991,
        title: "Sousou no Frieren",
        // U+2019, exactly as AniList serves it. Do NOT "fix" this to an ASCII
        // apostrophe: sites spell it ASCII, normalize_title drops ASCII
        // punctuation but passes U+2019 through, and the mismatch is why this
        // fixture cannot bind on a title-matched provider (ROD-526). Straighten
        // it here and the probe reports a match the app does not make.
        title_english: "Frieren: Beyond Journey\u{2019}s End",
        episodes: 28,
        status: "FINISHED",
        year: 2023,
    },
    Fixture {
        anilist_id: 1,
        mal_id: 1,
        title: "Cowboy Bebop",
        title_english: "Cowboy Bebop",
        episodes: 26,
        status: "FINISHED",
        year: 1998,
    },
    Fixture {
        anilist_id: 21,
        mal_id: 21,
        title: "One Piece",
        title_english: "ONE PIECE",
        // RELEASING, so the episode veto is spared: the count drifts every week
        // and a catalog listing 1136 against our 1100 is healthy, not a miss.
        episodes: 1100,
        status: "RELEASING",
        year: 1999,
    },
];

impl Fixture {
    fn enrichment(&self) -> Enrichment {
        Enrichment {
            anilist_id: self.anilist_id,
            mal_id: Some(self.mal_id),
            title_romaji: self.title.to_string(),
            title_english: Some(self.title_english.to_string()),
            total_episodes: Some(self.episodes),
            status: Some(self.status.to_string()),
            year: Some(self.year),
            ..Enrichment::default()
        }
    }
}

/// Two orderings, deliberately different. This one ranks which fixture best
/// represents a provider (`min` across fixtures), so Degraded outranks
/// NotStocked: it proves bind and list answered. Never reuse it for the
/// cross-provider rollup, which asks the opposite; `severity` owns that.
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

/// One provider against one fixture.
struct Probe {
    verdict: Verdict,
    stages: Vec<String>,
    detail: Option<String>,
    /// Bound by the fuzzy scorer, not an id: an exact title alone clears the
    /// score floor with zero corroboration, so the summary qualifies the OK.
    /// Ties take the first probe, so it warns pessimistically.
    fuzzy: bool,
}

impl Probe {
    fn graded(verdict: Verdict, stages: Vec<String>, detail: Option<String>) -> Probe {
        Probe {
            verdict,
            stages,
            detail,
            fuzzy: false,
        }
    }

    fn with_fuzzy(mut self, fuzzy: bool) -> Probe {
        self.fuzzy = fuzzy;
        self
    }
}

impl fmt::Display for Probe {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:<12}", self.verdict.label())?;
        write!(f, "{}", self.stages.join("  "))?;
        // Own line: appended inline, a long detail wraps the stage list
        // mid-token, which is where a real failure stops being readable.
        if let Some(detail) = &self.detail {
            write!(f, "\n  {:<20} {:<12}{detail}", "", "")?;
        }
        Ok(())
    }
}

fn secs(at: Instant) -> String {
    format!("{:.2}s", at.elapsed().as_secs_f64())
}

/// Every provider-controlled string reaching the terminal goes through here:
/// a CR or an ANSI escape in an id overwrites the row that would have shown a
/// real failure (`log_url` in providers/http.rs, same reasoning).
fn safe(s: &str) -> String {
    let mut out = strip_controls(s.to_string());
    if out.chars().count() > 80 {
        out = out.chars().take(80).collect::<String>() + "...";
    }
    out
}

/// Bind as the app binds (`workers::probe_candidate`): id match, then the fuzzy
/// scorer. `None` is a real answer; the walk would hop, so the probe must not
/// invent a binding the app would refuse. Taking `hits[0]` tests a path
/// production never runs and misgrades both ways.
fn pick_hit<'a>(hits: &'a [SearchHit], fx: &Fixture) -> Option<(&'a SearchHit, bool)> {
    let show = fx.enrichment();
    if let Some(ix) = resolver::best_id_match(&show, hits) {
        return Some((&hits[ix], true));
    }
    resolver::best_provider_match(&show, hits).map(|ix| (&hits[ix], false))
}

/// Ranged GET with the link's own headers. A 3xx is not reach: the transport
/// bans redirects (03 §6.7) so the destination is unknown, and a load-shed
/// bounce to an interstitial looks identical to a working CDN.
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

/// Anti-bot walls answer 200, so arrival proves nothing. Allowlist only: a
/// playlist naming at least one reference. Keep it an allowlist when adding
/// shapes; every blocklist here has been bypassed by a body nobody predicted.
fn sniff(body: &[u8]) -> Result<String, String> {
    if body.is_empty() {
        return Err("empty body (2xx with no bytes)".into());
    }
    let head = String::from_utf8_lossy(&body[..body.len().min(4096)]);
    // trim_start does not eat a BOM; one in front of a challenge page was a
    // free bypass of every check below.
    let text = head.trim_start_matches('\u{feff}').trim_start();
    if !text.starts_with("#EXTM3U") {
        return Err(format!(
            "unrecognized body, {}B (not a playlist)",
            body.len()
        ));
    }
    // The magic bytes are trivially forged; a real playlist names something.
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
    // The verdict no longer forks on how it bound; it rides to the summary so
    // a provider that only ever fuzzy-binds says so.
    let (id, fuzzy) = match provider.canonical_key(&show) {
        Some(key) => {
            stages.push(format!("bind:canonical({}) {}", safe(&key), secs(at)));
            (key, false)
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
                Ok(hits) => match pick_hit(&hits, fx) {
                    // Scorers refused every hit: the app would hop. Not
                    // stocked under a name we can match, not a site failure.
                    None => {
                        stages.push(format!(
                            "bind:search {} hits, no match {}",
                            hits.len(),
                            secs(at)
                        ));
                        return Probe::graded(
                            Verdict::NotStocked,
                            stages,
                            Some("search answered but nothing scored high enough to bind".into()),
                        );
                    }
                    Some((hit, keyed)) => {
                        let how = if keyed { "keyed" } else { "matched" };
                        stages.push(format!(
                            "bind:search {how}({}) {}",
                            safe(&hit.provider_id),
                            secs(at)
                        ));
                        (hit.provider_id.clone(), !keyed)
                    }
                },
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

    // Resolve can hand back a well-formed URL the CDN refuses to serve, which
    // is what a contract test sleeps through.
    let at = Instant::now();
    match reach(http, &link) {
        Err(e) => {
            stages.push(format!("reach {}", secs(at)));
            Probe::graded(Verdict::Degraded, stages, Some(format!("reach: {e}")))
        }
        Ok(note) => {
            stages.push(format!("reach:{note} {}", secs(at)));
            Probe::graded(Verdict::Ok, stages, None).with_fuzzy(fuzzy)
        }
    }
}

/// Best fixture wins: one success proves the provider, the rest were stocking
/// gaps. Except a clean sweep of absence, which given universally-stocked
/// fixtures is a catalog answering nothing rather than three delistings.
fn grade(probes: Vec<Probe>) -> (Verdict, Option<String>) {
    let swept = probes.iter().all(|p| p.verdict == Verdict::NotStocked);
    if swept {
        return (
            Verdict::Down,
            Some(format!(
                "every fixture unbindable ({} of {}); nothing in the catalog matched, not a delisting",
                probes.len(),
                FIXTURES.len()
            )),
        );
    }
    let best = probes
        .into_iter()
        .min_by_key(|p| p.verdict)
        .expect("FIXTURES is never empty");
    if best.verdict == Verdict::Ok && best.fuzzy {
        return (
            best.verdict,
            Some(
                "bound by title match, not by id: the stream is only as right as the scorer".into(),
            ),
        );
    }
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
    let shelf = retired_registry().map_err(|e| format!("registry: {e}"))?;
    let http = HttpClient::new().map_err(|e| format!("http client: {e}"))?;

    // A bare run grades the live set only. Shelved providers answer to an
    // explicit --provider so a full run never spends minutes on a site we
    // already stopped shipping.
    let providers: Vec<&dyn StreamProvider> = match &args.provider {
        Some(name) => vec![
            registry
                .by_name(name)
                .or_else(|| shelf.by_name(name))
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
    // Log sites are no-ops until a logger installs, so SABIGOKU_DEBUG needs
    // this. Keep it gated: unconditional, the always-on transport warns
    // interleave into the table and shred the report.
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

/// Examples need `--all-targets` to run these; plain `cargo test` skips them.
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
        let body = "\u{feff}<!DOCTYPE html><html>captcha</html>".as_bytes();
        assert!(sniff(body).is_err());
    }

    #[test]
    fn sniff_rejects_a_json_block_page() {
        assert!(sniff(br#"{"blocked":true,"reason":"bot"}"#).is_err());
    }

    #[test]
    fn sniff_rejects_a_playlist_header_with_no_references() {
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
        let (picked, keyed) = pick_hit(&hits, fx).expect("id match binds");
        assert_eq!(picked.provider_id, "right-show");
        assert!(keyed);
    }

    #[test]
    fn pick_hit_refuses_an_unrelated_hit_instead_of_taking_the_top_result() {
        // The old fallback bound hits[0] blindly, which is how a catalog that
        // answers every query with something unrelated graded OK. The scorers
        // refuse it, and a refusal is a real answer: the app would hop.
        let hits = vec![hit("whatever-was-first", None, None)];
        assert!(pick_hit(&hits, &FIXTURES[0]).is_none());
    }

    #[test]
    fn pick_hit_binds_a_title_match_with_no_ids_at_all() {
        // anineko carries neither id, so an id-only check could never bind it
        // however healthy it was. The fuzzy scorer is what makes it gradeable.
        let fx = &FIXTURES[1];
        let mut h = hit(fx.title, None, None);
        h.title = fx.title.to_string();
        h.total_episodes = Some(fx.episodes);
        let (picked, keyed) = pick_hit(std::slice::from_ref(&h), fx).expect("title match binds");
        assert_eq!(picked.provider_id, fx.title);
        assert!(!keyed, "no ids were present, so this cannot be id-keyed");
    }

    #[test]
    fn releasing_spares_the_episode_veto_that_a_finished_fixture_enforces() {
        // total_is_authoritative reads a MISSING status as authoritative, which
        // hard-rejects any candidate whose count is off by more than 3. One
        // Piece drifts weekly and the app spares it; drop `status` here and the
        // probe reports NOT-STOCKED on a catalog production binds fine.
        let op = &FIXTURES[2];
        let drifted = SearchHit {
            total_episodes: Some(op.episodes + 36),
            ..hit("op", Some(op.anilist_id), None)
        };
        assert!(
            resolver::best_id_match(&op.enrichment(), std::slice::from_ref(&drifted)).is_some(),
            "a still-airing show must tolerate a drifted count"
        );

        // The same drift against a FINISHED fixture is a different work, and
        // the veto must still fire.
        let done = &FIXTURES[0];
        let wrong = SearchHit {
            total_episodes: Some(done.episodes + 36),
            ..hit("f", Some(done.anilist_id), None)
        };
        assert!(
            resolver::best_id_match(&done.enrichment(), std::slice::from_ref(&wrong)).is_none(),
            "a finished show must reject a wildly wrong count"
        );

        // Literal counts, so each settled fixture's own number is pinned rather
        // than merely used. A drifted fixture would make the probe misreport
        // every catalog it grades.
        for (fx, real) in [(&FIXTURES[0], 28u32), (&FIXTURES[1], 26)] {
            let exact = SearchHit {
                total_episodes: Some(real),
                ..hit("x", Some(fx.anilist_id), None)
            };
            assert!(
                resolver::best_id_match(&fx.enrichment(), std::slice::from_ref(&exact)).is_some(),
                "fixture {} must carry the show's real episode count",
                fx.title
            );
        }
    }

    #[test]
    fn fixture_year_arms_a_veto_a_missing_one_leaves_dead() {
        // The year clause is NOT status-gated, so it guards even the releasing
        // fixture. With year unset it was dead code.
        // Literal years, NOT `op.year` offsets: a test written relative to the
        // fixture is invariant to the fixture and pins nothing. One Piece
        // premiered in 1999, so a candidate carrying 1999 must corroborate.
        let op = &FIXTURES[2];
        let right_era = SearchHit {
            year: Some(1999),
            ..hit("op", Some(op.anilist_id), None)
        };
        assert!(
            resolver::best_id_match(&op.enrichment(), std::slice::from_ref(&right_era)).is_some(),
            "the fixture year must agree with the show's real premiere"
        );
        let wrong_era = SearchHit {
            year: Some(2022),
            ..hit("op-movie", Some(op.anilist_id), None)
        };
        assert!(
            resolver::best_id_match(&op.enrichment(), std::slice::from_ref(&wrong_era)).is_none(),
            "an entry from another era must not ride the id agreement"
        );
    }

    #[test]
    fn a_title_matched_win_is_qualified_in_the_summary() {
        let fuzzy = vec![Probe::graded(Verdict::Ok, vec![], None).with_fuzzy(true)];
        let (verdict, detail) = grade(fuzzy);
        assert_eq!(verdict, Verdict::Ok);
        assert!(
            detail.is_some(),
            "a bind resting on title text alone must say so"
        );

        let keyed = vec![Probe::graded(Verdict::Ok, vec![], None).with_fuzzy(false)];
        let (verdict, detail) = grade(keyed);
        assert_eq!(verdict, Verdict::Ok);
        assert_eq!(detail, None);
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
