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
//! Read `--dub` grades loosely: a show with no dub fails at `resolve`, not at
//! listing, so plain absence lands as DEGRADED. Reclassifying it would mean
//! matching on provider error text, which breaks the moment a site rewords.

use std::fmt;
use std::process::ExitCode;
use std::time::Instant;

use sabigoku::domain::{Enrichment, Quality, StreamLink, Translation};
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

/// Ordering is severity, and `min` across fixtures is the provider's grade: one
/// fixture reaching Ok proves the provider works, whatever the others did.
/// Degraded outranks NotStocked because it carries more signal (bind and list
/// both answered), not because it is milder.
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

/// Prefer a hit the provider itself id-keyed to the fixture; fall back to the
/// site's top result. The fallback is reported, never silently trusted: a wrong
/// binding resolves fine and grades a provider healthy on the wrong show.
fn pick_hit<'a>(hits: &'a [SearchHit], fx: &Fixture) -> (&'a SearchHit, bool) {
    let keyed = hits
        .iter()
        .find(|h| h.anilist_id == Some(fx.anilist_id) || h.mal_id == Some(fx.mal_id));
    match keyed {
        Some(hit) => (hit, true),
        None => (&hits[0], false),
    }
}

/// Ranged GET with the link's own headers. A refused redirect is still reach:
/// the transport bans 3xx so a provider URL cannot escape the fetchguard
/// (03 §6.7), but mpv follows them, so grading one as unreachable would be a
/// lie about playback.
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
        Err(ProviderError::Http { status }) if (300..400).contains(&status) => {
            Ok(format!("{status} redirect"))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Status alone is not reach: a blocked CDN happily serves an interstitial as
/// 200, which would grade a dead provider healthy. Only an HTML body is called
/// a failure; anything not a playlist may still be a legitimate media fragment,
/// so it reports its size and lets the reader judge.
fn sniff(body: &[u8]) -> Result<String, String> {
    let head = String::from_utf8_lossy(&body[..body.len().min(512)]);
    let trimmed = head.trim_start();
    if trimmed.starts_with("#EXTM3U") {
        return Ok(format!("m3u8 {}B", body.len()));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("<!doctype") || lower.starts_with("<html") {
        return Err(format!("html body, {}B (not a stream)", body.len()));
    }
    Ok(format!("{}B", body.len()))
}

fn probe(provider: &dyn StreamProvider, fx: &Fixture, tt: Translation, http: &HttpClient) -> Probe {
    let mut stages: Vec<String> = Vec::new();
    let show = fx.enrichment();

    // Bind: tier A when the provider id-keys on canonical, else tier C search.
    let at = Instant::now();
    let id = match provider.canonical_key(&show) {
        Some(key) => {
            stages.push(format!("bind:canonical({key}) {}", secs(at)));
            key
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
                    return Probe::graded(Verdict::Down, stages, Some(format!("search: {e}")));
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
                        hit.provider_id,
                        secs(at)
                    ));
                    hit.provider_id.clone()
                }
            }
        }
    };

    // List: Ok(vec![]) is authoritative absence, never a failure (03 §4.3).
    let at = Instant::now();
    let labels = match provider.episodes(&id, tt, Some(fx.episodes)) {
        Err(e) => {
            stages.push(format!("list {}", secs(at)));
            return Probe::graded(Verdict::Down, stages, Some(format!("episodes: {e}")));
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
            return Probe::graded(Verdict::Degraded, stages, Some(format!("resolve: {e}")));
        }
        Ok(link) => {
            stages.push(format!("resolve:ep{} {}", labels[0], secs(at)));
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
        Ok(note) => {
            stages.push(format!("reach:{note} {}", secs(at)));
            Probe::graded(Verdict::Ok, stages, None)
        }
    }
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
        let mut best: Option<(Verdict, Option<String>)> = None;
        for fx in FIXTURES {
            let probe = probe(provider, fx, args.translation, &http);
            println!("  {:<20} {probe}", fx.title);
            if best.as_ref().is_none_or(|(v, _)| probe.verdict < *v) {
                best = Some((probe.verdict, probe.detail));
            }
        }
        let (verdict, detail) = best.expect("FIXTURES is never empty");
        println!("  verdict: {}\n", verdict.label());
        summary.push((provider.name(), verdict, detail));
    }

    println!("== summary ==");
    let mut overall = Verdict::Ok;
    for (name, verdict, detail) in &summary {
        let detail = detail.as_deref().unwrap_or("");
        println!("  {name:<12} {:<12} {detail}", verdict.label());
        overall = overall.max(*verdict);
    }
    Ok(overall)
}

fn main() -> ExitCode {
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
