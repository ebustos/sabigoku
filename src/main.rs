//! Bootstrap order per 01 §2: paths -> config -> store -> registry -> tui::run.
//! CLI dispatch (06 §7) runs before any of it; the Version, Usage and Update
//! arms never touch disk. Exit law (06 §7.4): login/sync/update/usage/version
//! exit 0 unconditionally; the play path is the one command whose failure exits
//! 1. Boot failures (no HOME, tui::run error) are a separate pre-existing 1.

use std::path::Path;
use std::process::ExitCode;

use sabigoku::aniskip;
use sabigoku::cli::{self, Command, FetchStage, PlayArgs};
use sabigoku::domain::{self, Enrichment, Quality, Translation};
use sabigoku::player::{self, PlayOpts, PlayerEvent};
use sabigoku::providers::{ProviderError, SearchHit, SearchOptions, StreamProvider};
use sabigoku::store::Store;
use sabigoku::tui::event::FetchClass;
use sabigoku::tui::workers::{finish_playback, play_failure};
use sabigoku::{config::Config, paths::Paths};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let debug = cli::debug_flag(&args);
    match cli::parse(&args) {
        Command::Version => {
            // Clean single line: the distribution contract (ROD-462) leans on
            // it, never a side effect of the usage fallthrough.
            println!("sabigoku v{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Paths => print_paths(),
        Command::Login { paste } => {
            sabigoku::logging::init_stderr(debug);
            run_login_cli(paste)
        }
        Command::Sync => {
            sabigoku::logging::init_stderr(debug);
            run_sync_cli()
        }
        Command::Update => {
            sabigoku::logging::init_stderr(debug);
            println!("update isn't available yet");
            ExitCode::SUCCESS
        }
        Command::Usage => {
            print!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        Command::Play(args) => {
            sabigoku::logging::init_stderr(debug);
            run_play_cli(args)
        }
        Command::Tui => run_tui(debug),
    }
}

fn print_paths() -> ExitCode {
    let paths = match Paths::resolve() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("sabigoku: {e}");
            return ExitCode::FAILURE;
        }
    };
    paths.ensure_dirs();
    let config = Config::load(&paths.config_file());
    let home = std::env::var("HOME").unwrap_or_default();
    let show = |p: &Path| sabigoku::paths::collapse_home(p, Path::new(&home));
    println!("sabigoku paths");
    println!("  config   {}", show(&paths.config_file()));
    println!("  db       {}", show(&paths.db_file()));
    println!("  cache    {}", show(&paths.cache));
    println!("  runtime  {}", show(&paths.runtime));
    println!("  mpv      {}", config.mpv_path);
    println!("  palette  {}", config.palette);
    ExitCode::SUCCESS
}

/// Paste line cap (zigoku parity: a JWT is ~1 KB; past 8 KB it is not one).
const PASTE_CAP: u64 = 8192;

/// `sabigoku login` (06 §4.3): loopback OAuth by default, `--paste` or a
/// bind/nonce failure falls back to manual paste. A persisted token (and only
/// that) chains the bootstrap sync (06 §4.5). Exits 0 on every branch.
fn run_login_cli(paste: bool) -> ExitCode {
    let paths = match Paths::resolve() {
        Ok(p) => p,
        Err(e) => {
            println!("  login: couldn't locate a config directory ({e})");
            return ExitCode::SUCCESS;
        }
    };
    paths.ensure_dirs();
    let auth_path = paths.auth_file();
    let client = match sabigoku::anilist::AniList::new() {
        Ok(c) => c,
        Err(e) => {
            println!("  login: couldn't set up the AniList client ({e})");
            return ExitCode::SUCCESS;
        }
    };

    let existing = sabigoku::auth::Auth::load(&auth_path);
    if existing.anilist.bearer().is_some() {
        println!(
            "Already signed in as {}; re-running replaces it.\n",
            existing.anilist.user_name
        );
    }

    let now = unix_now();
    let signed_in = if paste {
        paste_login(&client, &auth_path, now)
    } else {
        match sabigoku::loopback::Loopback::start() {
            Ok(lp) => loopback_login(&lp, &client, &auth_path, now),
            Err(_) => {
                println!("  (couldn't start the loopback listener; falling back to paste)\n");
                paste_login(&client, &auth_path, now)
            }
        }
    };

    if signed_in {
        println!();
        return run_sync_cli();
    }
    ExitCode::SUCCESS
}

fn loopback_login(
    lp: &sabigoku::loopback::Loopback,
    client: &sabigoku::anilist::AniList,
    auth_path: &Path,
    now: i64,
) -> bool {
    let url = lp.authorize_url();
    println!("Opening your browser to approve AniList access…");
    println!("  If it doesn't open, visit this URL yourself:");
    println!("  {url}\n");
    println!(
        "Waiting for the redirect on http://localhost:{}/ …",
        lp.port()
    );
    println!("  (Ctrl-C to cancel, or re-run `sabigoku login --paste` for manual entry.)\n");
    flush_stdout();
    sabigoku::login::open_browser(&url);

    let mut warned = false;
    let result = lp.serve(client, auth_path, now, || {
        if !warned {
            println!("  ⚠ ignoring callback(s) with a bad state (stray or forged requests).");
            flush_stdout();
            warned = true;
        }
    });
    print!("{}", cli::render_connect_result(&result, auth_path, false));
    matches!(result, sabigoku::login::ConnectResult::Ok { .. })
}

fn paste_login(client: &sabigoku::anilist::AniList, auth_path: &Path, now: i64) -> bool {
    println!("Connect your AniList account (OAuth Implicit Grant).\n");
    println!("1. Open this URL in a browser and approve:\n");
    println!("   {}\n", sabigoku::login::authorize_url_bare());
    println!(
        "2. You land on http://localhost:{}/…; paste that whole URL below. If nothing",
        sabigoku::login::LOOPBACK_PORT
    );
    println!("   is listening there the page won't load; that's fine, the token is in");
    println!("   the address bar. Select the ENTIRE URL (the token has three dot-");
    println!("   separated parts; a double-click grabs only the first).\n");
    print!("redirect URL> ");
    flush_stdout();

    let Some(line) = read_paste_line() else {
        println!("\n  no input (or a paste past 8 KB); aborted.");
        return false;
    };

    println!("\nverifying…");
    flush_stdout();
    let raw = sabigoku::login::normalize_paste(line.trim());
    let result = sabigoku::login::complete_login(&raw, client, auth_path, now);
    print!("{}", cli::render_connect_result(&result, auth_path, true));
    matches!(result, sabigoku::login::ConnectResult::Ok { .. })
}

/// One stdin line, capped. None on EOF, read error, or an overlong paste;
/// `cli::paste_line_usable` owns the accept/abort rule (08 §10 ratified).
fn read_paste_line() -> Option<String> {
    use std::io::{BufRead, BufReader, Read};
    let mut line = String::new();
    let n = BufReader::new(std::io::stdin().lock().take(PASTE_CAP))
        .read_line(&mut line)
        .ok()?;
    cli::paste_line_usable(n, &line, PASTE_CAP).then_some(line)
}

fn flush_stdout() {
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// `sabigoku sync` (06 §5.2 CLI row): pull then push, gated on token present +
/// unexpired only. Ignores `anilist_sync_enabled` on purpose (06 §5.5
/// asymmetry). Exits 0 on every path, setup failures included (zigoku runSync
/// swallowed even a store that won't open into a message).
fn run_sync_cli() -> ExitCode {
    let paths = match Paths::resolve() {
        Ok(p) => p,
        Err(e) => {
            println!("  sync: no local library to sync ({e})");
            return ExitCode::SUCCESS;
        }
    };
    paths.ensure_dirs();
    let store = match sabigoku::store::Store::open(&paths.db_file()) {
        Ok(s) => s,
        Err(e) => {
            println!("  sync: couldn't open the local library ({e})");
            return ExitCode::SUCCESS;
        }
    };
    let auth = sabigoku::auth::Auth::load(&paths.auth_file());
    let client = match sabigoku::anilist::AniList::new() {
        Ok(c) => c,
        Err(e) => {
            println!("  sync: couldn't set up the AniList client ({e})");
            return ExitCode::SUCCESS;
        }
    };

    let now = unix_now();
    if auth.anilist.bearer().is_some() && !auth.anilist.is_expired(now) {
        // Announce before the paced push; flush so it lands pre-network.
        println!("  syncing with AniList, this can take a moment…");
        flush_stdout();
    }
    match sabigoku::sync::run_sync(
        &client,
        &auth,
        &store,
        now,
        true,
        false,
        &sabigoku::sync::ThreadSleeper,
    ) {
        Ok(summary) => print!("{}", cli::render_sync_summary(&summary)),
        Err(e) => {
            log::debug!("sync: store error: {e}");
            println!(
                "  sync failed: couldn't update the local library; re-run with --debug for details."
            );
        }
    }
    ExitCode::SUCCESS
}

/// Numbered-pick read cap: a pick is a short number or `q`; past this a line is
/// garbage. Bounds the picker against an unbounded pipe.
const PICK_CAP: u64 = 256;

/// Reprompt ceiling: a human never fumbles a numbered pick this many times, but
/// a stdin flood (endless garbage or a non-UTF-8 stream with no newline) would
/// otherwise reprompt without end. Bounds the loop to abort instead.
const MAX_PICK_ATTEMPTS: usize = 1000;

/// `sabigoku <query>` (06 §7.3): the one command whose failure exits nonzero.
/// A single preferred provider, no fallback walk (03 §3.2); interactive stdin
/// picks; store is best-effort and degrades. This chunk covers search and the
/// show pick; episodes and playback land in the following chunks.
fn run_play_cli(args: PlayArgs) -> ExitCode {
    let paths = match Paths::resolve() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("sabigoku: {e}");
            return ExitCode::from(1);
        }
    };
    paths.ensure_dirs();
    let config = Config::load(&paths.config_file());
    let now = unix_now();

    // Translation is flag-only (zigoku parity): --dub/--sub decide it, config's
    // `translation` never reaches the query path. Don't "fix" this into reading
    // the config default; that would be a deviation, not a bug.
    let translation = if args.dub {
        Translation::Dub
    } else {
        Translation::Sub
    };

    // --quality is parsed but inert (06 §7): warn once on a non-default value so
    // the flag never looks silently honored. default_quality drives resolve.
    if cli::quality_note_needed(args.quality.as_deref()) {
        println!(
            "  (note: --quality isn't wired up yet; playback uses the highest direct stream available.)"
        );
    }

    // Store is best-effort: a library that won't open degrades to play-only (no
    // resume, episode cache, or history) and never blocks playback. Paired with
    // the hit's `anilist_id` below, `None` on either side skips every store hop.
    let store = match Store::open(&paths.db_file()) {
        Ok(s) => Some(s),
        Err(e) => {
            log::debug!("play: store open failed: {e}");
            println!("  (couldn't open your library; continuing without history.)");
            None
        }
    };

    let registry = match sabigoku::providers::default_registry() {
        Ok(r) => r,
        Err(e) => {
            println!("  ✗ couldn't set up a provider client ({e}).");
            return ExitCode::from(1);
        }
    };
    let provider = registry.preferred(Some(config.preferred_provider.as_str()));

    let hits = match provider.search(
        &args.query,
        &SearchOptions {
            translation,
            limit: 20,
            page: 1,
        },
    ) {
        Ok(hits) => hits,
        Err(e) => {
            print!(
                "{}",
                cli::fetch_error_line(FetchStage::Search, FetchClass::from(&e), provider.name())
            );
            return ExitCode::from(1);
        }
    };
    if hits.is_empty() {
        // Echoes raw argv; strip terminal-hostile bytes like every other
        // provider/user string reaching stdout on this path.
        println!(
            "\n  no results for \"{}\". try a different spelling or romaji.",
            domain::strip_controls(args.query.clone())
        );
        return ExitCode::SUCCESS;
    }

    print!("{}", cli::render_search_hits(&hits, translation));
    let Some(idx) = prompt_pick("\n  pick a show # (q to quit): ", hits.len()) else {
        println!("  bye.");
        return ExitCode::SUCCESS;
    };
    let hit = &hits[idx];

    // Persistence keys on the hit's anilist_id. Absent (senshi never carries
    // one; allanime sometimes) => play-only: episodes fetch fresh, no bind, no
    // cache, and later no resume or history.
    let anilist_id = hit.anilist_id;
    if let (Some(st), Some(id)) = (&store, anilist_id) {
        let enrichment = Enrichment {
            anilist_id: id,
            mal_id: hit.mal_id,
            // Provider-supplied title persists into a store row that every TUI
            // render trusts as pre-scrubbed (anilist ingestion strips on write;
            // this path must too, or it plants a replay-on-render injection).
            title_romaji: domain::strip_controls(hit.title.clone()),
            total_episodes: hit.total_episodes,
            ..Default::default()
        };
        // Best-effort warm; a bind miss must not sink the play (the show row is
        // preserved on conflict, so a synced library entry is never clobbered).
        if let Err(e) = st.bind_provider(&enrichment, provider.name(), &hit.provider_id, now) {
            log::debug!("play: bind_provider failed: {e}");
        }
    }

    let episodes = match load_episodes(store.as_ref(), anilist_id, provider, hit, translation, now)
    {
        Ok(eps) => eps,
        Err(e) => {
            print!(
                "{}",
                cli::fetch_error_line(FetchStage::Episodes, FetchClass::from(&e), provider.name())
            );
            return ExitCode::from(1);
        }
    };
    if episodes.is_empty() {
        println!(
            "\n  no {} episodes listed for this show.",
            translation.as_str()
        );
        return ExitCode::SUCCESS;
    }

    print!("{}", cli::render_episode_list(&episodes));
    let Some(ep_idx) = prompt_pick("\n  pick an episode # (q to quit): ", episodes.len()) else {
        println!("  bye.");
        return ExitCode::SUCCESS;
    };
    let episode = &episodes[ep_idx];
    let episode_index = (ep_idx + 1) as u32;

    // Resume start, gated on store+anilist_id. Computed caller-side per the
    // 03 §6.3.1 rewind rule; the player emits --start only when > 0.
    let start_secs = match (&store, anilist_id) {
        (Some(st), Some(id)) => st
            .get_resume(id, translation, episode)
            .ok()
            .flatten()
            .map_or(0.0, |r| r.start_secs(config.resume_offset_sec)),
        _ => 0.0,
    };
    if start_secs > 0.0 {
        println!("  ↺ resuming at {start_secs:.0}s");
    }

    // AniSkip keys on mal_id, independent of the store gate: a hit with a MAL id
    // but no AniList id still gets OP/ED skips (03 §9). A None mal plays plain.
    let skip = aniskip::prepare(
        hit.mal_id,
        aniskip::episode_number(episode, episode_index),
        aniskip::SkipMode::parse(&config.skip_mode),
        &paths.cache,
    );

    let title = domain::strip_controls(format!("{} · ep {episode}", hit.title));
    let opts = PlayOpts {
        mpv_path: &config.mpv_path,
        socket_dir: &paths.runtime,
        title: &title,
        start_secs,
        skip: skip.as_ref(),
    };
    // Quality rides config.default_quality; --quality is parsed-but-inert (06
    // §7 parity). Wiring the flag to resolve is a separate feature, not here.
    let quality = Quality::parse(&config.default_quality);

    println!(
        "\n  ▶ resolving ep {} ({}) and launching mpv…",
        domain::strip_controls(episode.clone()),
        translation.as_str()
    );
    flush_stdout();
    let outcome = player::play(
        &opts,
        || {
            provider
                .resolve(&hit.provider_id, episode, translation, quality)
                .map_err(Into::into)
        },
        |event| {
            if let PlayerEvent::Retry { attempt } = event {
                println!(
                    "  stream didn't open, retrying ({attempt}/{})",
                    player::MAX_PLAY_ATTEMPTS
                );
            }
        },
    );

    match outcome {
        Ok(out) => {
            // A meaningful watch returns Ok with a position even when mpv then
            // exits badly; persist it. finish_playback shuts its own gate on a
            // None position, so an empty watch writes nothing.
            if let (Some(st), Some(id)) = (&store, anilist_id)
                && let Err(e) = finish_playback(
                    st,
                    id,
                    translation,
                    episode,
                    episode_index,
                    out.position,
                    Some(provider.name()),
                    unix_now(),
                )
            {
                log::debug!("play: finish_playback failed: {e}");
            }
            println!("\n  ✓ done.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            // The one nonzero exit (06 §7.4): a play that never yielded a
            // meaningful watch. Meaningful-watch mpv failures fold into Ok above.
            print!(
                "{}",
                cli::player_failure_line(play_failure(&e), provider.name())
            );
            ExitCode::from(1)
        }
    }
}

/// Cache-first episodes (zigoku ROD-68): an unexpired store hit wins; a miss
/// fetches from the provider and warms the cache. Both cache read and write are
/// gated on a store AND an anilist key; without either, always fetch fresh.
/// Cache read/warm failures degrade to a plain fetch, never an error.
fn load_episodes(
    store: Option<&Store>,
    anilist_id: Option<i64>,
    provider: &dyn StreamProvider,
    hit: &SearchHit,
    translation: Translation,
    now: i64,
) -> Result<Vec<String>, ProviderError> {
    let title = domain::strip_controls(hit.title.clone());
    if let (Some(st), Some(id)) = (store, anilist_id)
        && let Ok(Some(cached)) = st.get_cached_episodes(id, provider.name(), translation, now)
    {
        println!("\n  episodes for \"{title}\" (cached)");
        return Ok(cached);
    }
    println!("\n  fetching episodes for \"{title}\"…");
    flush_stdout();
    let episodes = provider.episodes(&hit.provider_id, translation, hit.total_episodes)?;
    if let (Some(st), Some(id)) = (store, anilist_id) {
        // Airing status is unknown on the query path (no AniList media row), so
        // the cache lands with the default TTL. A warm miss is inert.
        if let Err(e) = st.set_episode_cache(id, provider.name(), translation, &episodes, None, now)
        {
            log::debug!("play: episode cache warm failed: {e}");
        }
    }
    Ok(episodes)
}

/// Numbered stdin pick loop: prints the prompt, reads one capped line, and lets
/// `cli::classify_pick` own the accept/reprompt/abort rule. `None` on `q`, EOF,
/// or an overlong line. One locked reader spans the loop so buffered bytes
/// survive between reprompts.
fn prompt_pick(prompt: &str, max: usize) -> Option<usize> {
    use std::io::{BufRead, Read};
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    // Bounded, not `loop`: every non-exiting branch below (blank, bad number,
    // decode error) reprompts, so an endless garbage stream must not spin here.
    for _ in 0..MAX_PICK_ATTEMPTS {
        print!("{prompt}");
        flush_stdout();
        let mut line = String::new();
        match (&mut reader).take(PICK_CAP).read_line(&mut line) {
            // EOF: same as an explicit `q`.
            Ok(0) => return None,
            // A cap-length read with no newline is a truncated flood, not a
            // pick (a real choice is a few bytes); abort, matching zigoku
            // promptChoice's StreamTooLong -> quit.
            Ok(n) if n as u64 == PICK_CAP && !line.ends_with('\n') => return None,
            Ok(_) => match cli::classify_pick(&line, max) {
                cli::PickInput::Pick(i) => return Some(i),
                cli::PickInput::Abort => return None,
                cli::PickInput::Reprompt => {}
                cli::PickInput::NotNumber => println!("  ? enter a number 1-{max} (or q)"),
                cli::PickInput::OutOfRange => println!("  ? out of range: pick 1-{max}"),
            },
            // A decode error (non-UTF-8 bytes) is not EOF; the read still
            // advanced past them, so reprompt rather than silently quitting.
            Err(_) => println!("  ? couldn't read that (bad input encoding); try again"),
        }
    }
    // Ran out of patience: a flood, not a user. Abort like EOF.
    None
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn run_tui(debug: bool) -> ExitCode {
    let paths = match Paths::resolve() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("sabigoku: {e}");
            return ExitCode::FAILURE;
        }
    };
    paths.ensure_dirs();
    let config = Config::load(&paths.config_file());

    // Failure is stderr-reportable here; once tui::run owns the terminal it
    // would punch the frame. The handle's drop shuts the sink down.
    let _log = match sabigoku::logging::init(&paths.data, debug) {
        Ok(handle) => Some(handle),
        Err(e) => {
            eprintln!("sabigoku: log sink unavailable: {e}");
            sabigoku::logging::init_stderr(debug);
            None
        }
    };

    if let Err(e) = sabigoku::tui::run(&paths, &config) {
        eprintln!("sabigoku: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
