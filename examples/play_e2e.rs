//! play_e2e: the ROD-437 exit proof, end to end on the real seams. Search a
//! live provider, resolve an episode, play it through player::play (guard,
//! engage, IPC, retry), checkpoint progress, then show the partial watch
//! landing in History. Thread shape mirrors 439: play on a worker, events and
//! store writes on the main thread. In-memory store; nothing durable touched.
//!
//! Run:  cargo run --example play_e2e -- frieren
//!       cargo run --example play_e2e -- --provider megaplay --episode 3 "one piece"
//!       cargo run --example play_e2e -- --translation dub frieren

use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sabigoku::aniskip;
use sabigoku::config::Config;
use sabigoku::domain::{Enrichment, Quality, Translation};
use sabigoku::paths::Paths;
use sabigoku::player::{self, PlayOpts, PlayerEvent};
use sabigoku::providers::megaplay::MegaPlay;
use sabigoku::providers::senshi::Senshi;
use sabigoku::providers::{SearchHit, SearchOptions, StreamProvider};
use sabigoku::store::Store;
use sabigoku::tui::workers::finish_playback;

type Err = Box<dyn std::error::Error>;

/// In-memory db only; providers carry no AniList id, and the canonical-key
/// walk is out of 437's scope (harness plays from an already-resolved URL).
const FAKE_ANILIST_ID: i64 = 900_000_001;

const CHECKPOINT_PERIOD: Duration = Duration::from_secs(30);
const PRINT_PERIOD: Duration = Duration::from_secs(1);

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

struct Args {
    provider: String,
    episode: Option<String>,
    translation: Translation,
    /// Tier-A key for search-less providers (megaplay); routed through
    /// `canonical_key` like the real classifier, never hardcoded.
    mal: Option<i64>,
    /// Count hint for listing-less grids (03 §4.3).
    eps: Option<u32>,
    query: String,
}

fn parse_args() -> Result<Args, Err> {
    let mut provider = "senshi".to_string();
    let mut episode = None;
    let mut translation = Translation::Sub;
    let mut mal = None;
    let mut eps = None;
    let mut query_parts: Vec<String> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--provider" => provider = args.next().ok_or("--provider needs a value")?,
            "--episode" => episode = Some(args.next().ok_or("--episode needs a value")?),
            "--mal" => mal = Some(args.next().ok_or("--mal needs a value")?.parse()?),
            "--eps" => eps = Some(args.next().ok_or("--eps needs a value")?.parse()?),
            "--translation" => {
                translation = match args.next().as_deref() {
                    Some("sub") => Translation::Sub,
                    Some("dub") => Translation::Dub,
                    other => return Err(format!("bad --translation {other:?}").into()),
                }
            }
            other => query_parts.push(other.to_string()),
        }
    }
    if query_parts.is_empty() {
        return Err("usage: play_e2e [--provider senshi|megaplay] [--episode N|label] [--translation sub|dub] [--mal ID] [--eps N] <query>".into());
    }
    Ok(Args {
        provider,
        episode,
        translation,
        mal,
        eps,
        query: query_parts.join(" "),
    })
}

/// Episode remap parity (03 §6.6): exact raw label, else 1-based ordinal.
fn pick_episode(labels: &[String], wanted: Option<&str>) -> Option<usize> {
    let wanted = wanted.unwrap_or("1");
    if let Some(at) = labels.iter().position(|l| l == wanted) {
        return Some(at);
    }
    let ordinal: usize = wanted.parse().ok()?;
    (1..=labels.len()).contains(&ordinal).then(|| ordinal - 1)
}

fn run() -> Result<(), Err> {
    let args = parse_args()?;
    let provider: Box<dyn StreamProvider> = match args.provider.as_str() {
        "senshi" => Box::new(Senshi::new()?),
        "megaplay" => Box::new(MegaPlay::new()?),
        other => return Err(format!("unknown provider {other}").into()),
    };
    let translation = args.translation;

    let hit = if let Some(mal) = args.mal {
        let key = provider
            .canonical_key(&Enrichment {
                anilist_id: FAKE_ANILIST_ID,
                mal_id: Some(mal),
                ..Default::default()
            })
            .ok_or("provider does not key on MAL id")?;
        println!("tier-A key on {}: mal {mal} -> id {key}", provider.name());
        SearchHit {
            provider_id: key,
            title: args.query.clone(),
            total_episodes: args.eps,
            ..Default::default()
        }
    } else {
        println!("searching {} for {:?} ...", provider.name(), args.query);
        let hits = provider.search(
            &args.query,
            &SearchOptions {
                translation,
                limit: 20,
                page: 1,
            },
        )?;
        hits.first().ok_or("no search hits")?.clone()
    };
    let track_count = match translation {
        Translation::Sub => hit.eps_sub,
        Translation::Dub => hit.eps_dub,
    };
    println!(
        "  hit: {:?}  id={}  ({track_count} {} eps)",
        hit.title,
        hit.provider_id,
        translation.as_str(),
    );

    let labels = provider.episodes(&hit.provider_id, translation, hit.total_episodes)?;
    let at = pick_episode(&labels, args.episode.as_deref())
        .ok_or_else(|| format!("episode {:?} not in {} labels", args.episode, labels.len()))?;
    let label = labels[at].clone();
    let episode_index = (at + 1) as u32;
    println!("  episode {label:?} ({episode_index} of {})", labels.len());

    let store = Store::open_memory()?;
    let enrichment = Enrichment {
        anilist_id: FAKE_ANILIST_ID,
        title_romaji: hit.title.clone(),
        total_episodes: hit.total_episodes,
        ..Default::default()
    };
    store.bind_provider(&enrichment, provider.name(), &hit.provider_id, now())?;

    let config = Config::default();
    let start_secs = store
        .get_resume(FAKE_ANILIST_ID, translation, &label)?
        .map_or(0.0, |resume| resume.start_secs(config.resume_offset_sec));

    let paths = Paths::resolve()?;
    paths.ensure_dirs();

    // AniSkip once before the attempts, worker-shape parity (04 §7.8);
    // best-effort, a miss plays plain.
    let skip = aniskip::prepare(
        args.mal,
        aniskip::episode_number(&label, episode_index),
        aniskip::SkipMode::parse(&config.skip_mode),
        &paths.cache,
    );
    if let Some(s) = &skip {
        println!("aniskip: {}", s.opts);
    }

    let (event_tx, event_rx) = mpsc::channel::<PlayerEvent>();
    let title = format!("{} · {label}", hit.title);
    let provider_id = hit.provider_id.clone();
    let worker_label = label.clone();
    let worker = std::thread::spawn(move || {
        let opts = PlayOpts {
            mpv_path: &config.mpv_path,
            socket_dir: &paths.runtime,
            title: &title,
            start_secs,
            skip: skip.as_ref(),
        };
        let on_event = move |event| {
            let _ = event_tx.send(event);
        };
        player::play(
            &opts,
            || {
                provider
                    .resolve(&provider_id, &worker_label, translation, Quality::Best)
                    .map_err(Into::into)
            },
            on_event,
        )
    });

    println!("resolving + spawning mpv (resolve runs per attempt) ...");
    let mut last_print = Instant::now() - PRINT_PERIOD;
    let mut last_checkpoint = Instant::now();
    while let Ok(event) = event_rx.recv() {
        match event {
            PlayerEvent::Position(p) => {
                if last_print.elapsed() >= PRINT_PERIOD {
                    match p.duration {
                        Some(d) if d > 0.0 => {
                            println!(
                                "  {:7.1}s / {:.1}s  ({:.0}%)",
                                p.secs,
                                d,
                                100.0 * p.secs / d
                            )
                        }
                        _ => println!("  {:7.1}s / ?", p.secs),
                    }
                    last_print = Instant::now();
                }
                // Meaningful only: a 0/NaN checkpoint would clobber a real resume.
                if p.secs.is_finite()
                    && p.secs > 0.0
                    && last_checkpoint.elapsed() >= CHECKPOINT_PERIOD
                {
                    store.save_progress(
                        FAKE_ANILIST_ID,
                        translation,
                        &label,
                        p.secs,
                        p.duration.unwrap_or(0.0),
                        Some(args.provider.as_str()),
                        now(),
                    )?;
                    last_checkpoint = Instant::now();
                    println!("  checkpoint saved");
                }
            }
            PlayerEvent::Retry { attempt } => {
                println!(
                    "  stream didn't open, retrying (attempt {attempt}/{})",
                    player::MAX_PLAY_ATTEMPTS
                )
            }
        }
    }

    let outcome = worker.join().expect("play worker panicked")?;
    println!(
        "mpv done: attempts={} final={:?}",
        outcome.attempts, outcome.position
    );

    let recorded = finish_playback(
        &store,
        FAKE_ANILIST_ID,
        translation,
        &label,
        episode_index,
        outcome.position,
        Some(args.provider.as_str()),
        now(),
    )?;
    println!(
        "recordPlay gate: {}",
        if recorded {
            "OPEN (recorded)"
        } else {
            "shut (no writes)"
        }
    );

    println!("\nHistory:");
    let history = store.list_history()?;
    if history.is_empty() {
        println!("  (empty)");
    }
    for show in &history {
        println!(
            "  {} · plays {} · progress {} · last watched {:?}",
            show.enrichment.title_romaji, show.play_count, show.progress, show.last_watched_at
        );
    }
    if let Some(resume) = store.get_resume(FAKE_ANILIST_ID, translation, &label)? {
        println!(
            "resume row: {:.1}s / {:.1}s  fully_watched={}",
            resume.position_secs, resume.duration_secs, resume.fully_watched
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("play_e2e: {e}");
            ExitCode::FAILURE
        }
    }
}
