//! Bootstrap order per 01 §2: paths -> config -> store -> registry -> tui::run.
//! CLI dispatch (06 §7) runs before any of it; only the Tui and Paths arms
//! touch disk. Exit law (06 §7.4): the play path owns the binary's only
//! deliberate nonzero exit; boot failures (no HOME, tui::run error) stay 1.

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
        Command::Usage => {
            print!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        Command::Login { .. } => {
            sabigoku::logging::init_stderr(debug);
            println!("login isn't available yet");
            ExitCode::SUCCESS
        }
        Command::Sync => {
            sabigoku::logging::init_stderr(debug);
            println!("sync isn't available yet");
            ExitCode::SUCCESS
        }
        Command::Update => {
            sabigoku::logging::init_stderr(debug);
            println!("update isn't available yet");
            ExitCode::SUCCESS
        }
        Command::Play(_) => {
            sabigoku::logging::init_stderr(debug);
            eprintln!("sabigoku: non-TUI play isn't supported yet");
            ExitCode::from(2)
        }
        Command::Paths => print_paths(),
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
