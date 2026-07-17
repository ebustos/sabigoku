//! Bootstrap order per 01 §2: paths -> config -> store -> registry -> tui::run.
//! Only the first two exist; the rest arrive with M1. Spikes live in `src/bin/`.

use std::path::Path;

use sabigoku::{config::Config, paths::Paths};

fn main() {
    let paths = match Paths::resolve() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("sabigoku: {e}");
            std::process::exit(1);
        }
    };
    paths.ensure_dirs();
    let config = Config::load(&paths.config_file());

    let home = std::env::var("HOME").unwrap_or_default();
    let show = |p: &Path| sabigoku::paths::collapse_home(p, Path::new(&home));
    println!("sabigoku skeleton (M1.1)");
    println!("  config   {}", show(&paths.config_file()));
    println!("  db       {}", show(&paths.db_file()));
    println!("  cache    {}", show(&paths.cache));
    println!("  runtime  {}", show(&paths.runtime));
    println!("  mpv      {}", config.mpv_path);
    println!("  palette  {}", config.palette);
}
