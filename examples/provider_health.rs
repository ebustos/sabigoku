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
    /// Verbatim AniList `title.english`. The scorer reads every title form, so
    /// omitting it makes a tier-C provider look worse here than in the app.
    title_english: &'static str,
    /// `count_hint` for listing-less providers (03 §4.3).
    episodes: u32,
    /// Verbatim AniList `status`. Not decoration: `total_is_authoritative`
    /// reads None as authoritative, which hard-rejects any candidate whose
    /// count differs by more than 3. A still-airing show is spared that in the
    /// app, so leaving this unset makes the probe stricter than production.
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
    /// Bound by the fuzzy scorer rather than an id. Carried to the summary so a
    /// provider whose every bind rests on title text cannot report a bare OK:
    /// an exact title alone clears the score floor with zero corroboration.
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
        // Own line, indented past the title column: appended inline, a long
        // detail pushes the row past the terminal and the stage list wraps
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

/// Bind exactly as the app binds (`workers::probe_candidate`): id match first,
/// then the fuzzy scorer. `None` is a real answer, the walk would hop rather
/// than bind, so the probe must not invent a binding the app would refuse.
///
/// Taking `hits[0]` instead would test a path production never runs. It also
/// misgrades both ways: a provider that cannot id-key at all (anineko carries
/// no ids by design) could never clear an id check, while a catalog answering
/// every query with an unrelated top result would resolve it and grade OK.
/// `best_provider_match` is the defense against the latter, and it is the same
/// one the app relies on, so the probe should not hand-roll a weaker version.
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
    // The verdict no longer forks on how it bound, because the scorers already
    // refuse what an id check was standing in for. It still rides to the
    // summary, so a provider that only ever fuzzy-binds says so.
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
                    // Hits came back and the scorers refused all of them. The
                    // app would hop, so this is "not stocked under a name we
                    // can match", not a failure to report against the site.
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
                // Says "nothing bindable" rather than "answers nothing": a
                // catalog that returns plenty of hits the scorers all refuse
                // reaches this same sweep, and it did answer.
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
