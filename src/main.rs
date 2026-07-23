//! Bootstrap order per 01 §2: paths -> config -> store -> registry -> tui::run.
//! CLI dispatch (06 §7) runs before any of it; the Version, Usage and Update
//! arms never touch disk. Exit law (06 §7.4) governs command outcomes: login/sync/update/
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

/// One stdin line, capped. None on EOF, read error, or an overlong paste (a
/// cap-length read with no newline can only be truncation).
fn read_paste_line() -> Option<String> {
    use std::io::{BufRead, BufReader, Read};
    let mut line = String::new();
    let n = BufReader::new(std::io::stdin().lock().take(PASTE_CAP))
        .read_line(&mut line)
        .ok()?;
    if n == 0 || (!line.ends_with('\n') && line.len() as u64 >= PASTE_CAP) {
        return None;
    }
    Some(line)
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
