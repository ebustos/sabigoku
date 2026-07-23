//! Bootstrap order per 01 §2: paths -> config -> store -> registry -> tui::run.
//! CLI dispatch (06 §7) runs before any of it; only the Tui, Paths and Sync
//! arms touch disk. Exit law (06 §7.4) governs command outcomes: login/sync/update/
//! usage/version exit 0, the play path's interim stub exits 2 (ROD-473 makes
//! it 1, the law's only nonzero). Boot failures (no HOME, tui::run error) are
//! a separate pre-existing 1, outside that law.

use std::path::Path;
use std::process::ExitCode;

use sabigoku::cli::{self, Command};
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
        Command::Login { .. } => {
            sabigoku::logging::init_stderr(debug);
            println!("login isn't available yet");
            ExitCode::SUCCESS
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
        Command::Play(_) => {
            sabigoku::logging::init_stderr(debug);
            eprintln!("sabigoku: non-TUI play isn't supported yet");
            ExitCode::from(2)
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
        let _ = std::io::Write::flush(&mut std::io::stdout());
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
