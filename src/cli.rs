//! CLI dispatch (06 §7). Subcommand = first non-flag positional matching a
//! known name; flags may precede it; after a real query word, subcommand names
//! are search text. Exit law (06 §7.4): the query play path owns the binary's
//! only nonzero exit; every other path, usage and bad flags included, exits 0.
//! `parse` is pure over argv (without argv0); main owns process exit.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Tui,
    Version,
    Paths,
    Usage,
    Login { paste: bool },
    Sync,
    Update,
    Play(PlayArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayArgs {
    pub query: String,
    pub dub: bool,
    /// Raw value, parity-inert (default_quality drives playback); mapping and
    /// wiring are ROD-473's call.
    pub quality: Option<String>,
}

/// `--debug` is global and consumed by every path (06 §7.2); read it before
/// dispatch so both sinks (stderr CLI, file TUI) see it.
pub fn debug_flag(args: &[String]) -> bool {
    args.iter().any(|a| a == "--debug")
}

pub fn parse(args: &[String]) -> Command {
    // Version outranks everything, even a bad flag or a query (zigoku ROD-221:
    // never routed through the usage fallthrough).
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Command::Version;
    }
    if args.iter().any(|a| a == "--paths") {
        return Command::Paths;
    }
    if is_subcommand(args, "login") {
        return Command::Login {
            paste: args.iter().any(|a| a == "--paste"),
        };
    }
    if is_subcommand(args, "sync") {
        return Command::Sync;
    }
    if is_subcommand(args, "update") {
        return Command::Update;
    }
    parse_query(args)
}

/// First non-flag positional equals `name`; any other positional ends the scan.
fn is_subcommand(args: &[String], name: &str) -> bool {
    for a in args {
        if a == name {
            return true;
        }
        if !a.starts_with('-') {
            return false;
        }
    }
    false
}

fn parse_query(args: &[String]) -> Command {
    let mut words: Vec<&str> = Vec::new();
    let mut dub = false;
    let mut quality: Option<String> = None;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--dub" {
            dub = true;
        } else if a == "--sub" {
            dub = false;
        } else if a == "--quality" {
            match it.next() {
                Some(v) => quality = Some(v.clone()),
                None => return Command::Usage,
            }
        } else if let Some(v) = a.strip_prefix("--quality=") {
            quality = Some(v.to_string());
        } else if a == "--debug" {
            // Global, consumed: not a query word, not an unknown flag.
        } else if a.starts_with("--") {
            return Command::Usage;
        } else {
            // Single-dash words fall through to the query (zigoku parity:
            // only `--` prefixes are flags here).
            words.push(a.as_str());
        }
    }

    if words.is_empty() {
        return Command::Tui;
    }
    Command::Play(PlayArgs {
        query: words.join(" "),
        dub,
        quality,
    })
}

/// All commands listed, a deliberate deviation from zigoku's query-only usage
/// (ratified with the ROD-470 scope).
pub const USAGE: &str = "  usage: sabigoku <query> [--dub] [--quality <q>] [--debug]
         sabigoku login [--paste]
         sabigoku sync
         sabigoku update
         sabigoku --version

    sabigoku frieren
    sabigoku \"cowboy bebop\" --dub
    sabigoku login

  --version (or -V) prints the version and exits.
  --paths prints the config/data/cache locations and exits.
  --debug (or SABIGOKU_DEBUG=1) writes diagnostics: stderr in CLI mode,
  ~/.local/share/sabigoku/sabigoku.log in the TUI.
";

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_of(args: &[&str]) -> Command {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        parse(&owned)
    }

    #[test]
    fn no_args_and_bare_flags_launch_the_tui() {
        assert_eq!(parse_of(&[]), Command::Tui);
        assert_eq!(parse_of(&["--dub"]), Command::Tui);
        assert_eq!(parse_of(&["--debug", "--sub"]), Command::Tui);
    }

    #[test]
    fn version_flag_wins_anywhere_even_over_a_query_or_bad_flag() {
        assert_eq!(parse_of(&["--version"]), Command::Version);
        assert_eq!(parse_of(&["-V"]), Command::Version);
        assert_eq!(parse_of(&["frieren", "--version"]), Command::Version);
        assert_eq!(parse_of(&["--nope", "-V"]), Command::Version);
    }

    #[test]
    fn flags_may_precede_a_subcommand() {
        assert_eq!(
            parse_of(&["--debug", "login"]),
            Command::Login { paste: false }
        );
        assert_eq!(
            parse_of(&["--paste", "login"]),
            Command::Login { paste: true }
        );
        assert_eq!(
            parse_of(&["login", "--paste"]),
            Command::Login { paste: true }
        );
        assert_eq!(parse_of(&["--debug", "sync"]), Command::Sync);
        assert_eq!(parse_of(&["update"]), Command::Update);
    }

    #[test]
    fn after_a_query_word_subcommand_names_are_search_text() {
        assert_eq!(
            parse_of(&["frieren", "login"]),
            Command::Play(PlayArgs {
                query: "frieren login".into(),
                dub: false,
                quality: None,
            })
        );
        assert_eq!(
            parse_of(&["cowboy", "sync"]),
            Command::Play(PlayArgs {
                query: "cowboy sync".into(),
                dub: false,
                quality: None,
            })
        );
    }

    #[test]
    fn translation_flags_apply_and_the_last_one_wins() {
        assert_eq!(
            parse_of(&["x", "--dub"]),
            Command::Play(PlayArgs {
                query: "x".into(),
                dub: true,
                quality: None,
            })
        );
        assert_eq!(
            parse_of(&["x", "--dub", "--sub"]),
            Command::Play(PlayArgs {
                query: "x".into(),
                dub: false,
                quality: None,
            })
        );
    }

    #[test]
    fn quality_takes_both_forms_and_a_missing_value_is_usage() {
        let want = Command::Play(PlayArgs {
            query: "x".into(),
            dub: false,
            quality: Some("1080".into()),
        });
        assert_eq!(parse_of(&["x", "--quality", "1080"]), want);
        assert_eq!(parse_of(&["x", "--quality=1080"]), want);
        assert_eq!(parse_of(&["x", "--quality"]), Command::Usage);
    }

    #[test]
    fn paths_flag_outranks_a_subcommand() {
        assert_eq!(parse_of(&["login", "--paths"]), Command::Paths);
        assert_eq!(parse_of(&["--paths", "sync"]), Command::Paths);
    }

    #[test]
    fn unknown_double_dash_flag_is_usage_but_single_dash_is_query_text() {
        assert_eq!(parse_of(&["--nope"]), Command::Usage);
        assert_eq!(parse_of(&["frieren", "--nope"]), Command::Usage);
        // --paste is only login's flag; elsewhere it is an unknown flag.
        assert_eq!(parse_of(&["--paste"]), Command::Usage);
        assert_eq!(
            parse_of(&["-x"]),
            Command::Play(PlayArgs {
                query: "-x".into(),
                dub: false,
                quality: None,
            })
        );
    }

    #[test]
    fn debug_flag_is_consumed_globally_and_detected() {
        let args: Vec<String> = vec!["frieren".into(), "--debug".into()];
        assert!(debug_flag(&args));
        assert_eq!(
            parse(&args),
            Command::Play(PlayArgs {
                query: "frieren".into(),
                dub: false,
                quality: None,
            })
        );
        assert!(!debug_flag(&["frieren".to_string()]));
    }
}
