//! CLI dispatch (06 §7). Subcommand = first non-flag positional matching a
//! known name; flags may precede it; after a real query word, subcommand names
//! are search text. Exit law (06 §7.4): the query play path owns the binary's
//! only nonzero exit; every other path, usage and bad flags included, exits 0.
//! `parse` is pure over argv (without argv0); main owns process exit. The
//! sync summary renderer lives here too: pure data to text, main prints.

use crate::sync::{SyncOutcome, SyncSummary};

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

/// Inline show/id lines before "… and N more" (zigoku SHOW_LIST_CAP).
const SHOW_LIST_CAP: usize = 12;

/// `sabigoku sync` human summary (zigoku runSync wording, `sabigoku login`
/// substituted, punctuation ours). zigoku's engaged-but-unlinked listing has
/// no counterpart: every library row here is keyed by `anilist_id`.
pub fn render_sync_summary(s: &SyncSummary) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    match s.outcome {
        // Disabled is unreachable from the CLI (it ignores the master switch,
        // 06 §5.5); worded as not-connected to stay total.
        SyncOutcome::NoToken | SyncOutcome::Disabled => {
            return "  not connected: run `sabigoku login` first.\n".into();
        }
        SyncOutcome::Expired => {
            return "  your AniList token has expired: run `sabigoku login` to reconnect.\n".into();
        }
        SyncOutcome::NoUserId => {
            return "  sync skipped: can't tell which AniList account this token is for; \
                    run `sabigoku login` to reconnect.\n"
                .into();
        }
        SyncOutcome::PullUnauthorized => {
            return "  pull stopped: AniList rejected the token; run `sabigoku login` to reconnect.\n"
                .into();
        }
        SyncOutcome::PullRateLimited => {
            return "  pull stopped: hit AniList's rate limit; run `sabigoku sync` again shortly.\n"
                .into();
        }
        SyncOutcome::Failed => {
            return "  sync failed: couldn't reach the local library or AniList.\n".into();
        }
        SyncOutcome::Completed | SyncOutcome::Unauthorized | SyncOutcome::RateLimited => {}
    }

    if s.pull_failed {
        out.push_str(
            "  pull failed: couldn't fetch your AniList list; re-run with --debug for details.\n",
        );
    } else {
        if s.pulled.reconciled > 0 {
            let _ = writeln!(
                out,
                "  pulled {} update(s) from AniList.",
                s.pulled.reconciled
            );
        } else if s.pulled.conflicts == 0 && s.pulled.imported == 0 {
            out.push_str("  already up to date: nothing to pull in.\n");
        }
        if s.pulled.imported > 0 {
            let _ = writeln!(
                out,
                "  imported {} show(s) from AniList into your library.",
                s.pulled.imported
            );
        }
        if s.pulled.conflicts > 0 {
            let _ = writeln!(
                out,
                "  ({} show(s) kept your local status over AniList's; they'll push back up next sync.)",
                s.pulled.conflicts
            );
        }
        if s.pulled.contended > 0 {
            let _ = writeln!(
                out,
                "  ({} show(s) changed mid-sync; left as-is, will reconcile next run.)",
                s.pulled.contended
            );
        }
        if !s.pulled.unmatched.is_empty() {
            let _ = writeln!(
                out,
                "  ({} AniList show(s) aren't in your local library yet; not imported.)",
                s.pulled.unmatched.len()
            );
            let shown = s.pulled.unmatched.len().min(SHOW_LIST_CAP);
            for id in &s.pulled.unmatched[..shown] {
                let _ = writeln!(out, "      · anilist.co/anime/{id}");
            }
            if s.pulled.unmatched.len() > shown {
                let _ = writeln!(out, "      … and {} more", s.pulled.unmatched.len() - shown);
            }
        }
    }

    if s.dirty == 0 {
        out.push_str("  already up to date: nothing to push.\n");
    } else {
        let _ = writeln!(
            out,
            "  pushed {} of {} change(s) to AniList.",
            s.pushed, s.dirty
        );
    }
    if s.push_failed > 0 {
        let _ = writeln!(
            out,
            "  {} push(es) failed; re-run with --debug for details.",
            s.push_failed
        );
    }
    match s.outcome {
        SyncOutcome::Unauthorized => out.push_str(
            "  stopped: AniList rejected the token mid-run; run `sabigoku login` to reconnect.\n",
        ),
        SyncOutcome::RateLimited => out.push_str(
            "  stopped: hit AniList's rate limit; run `sabigoku sync` again shortly to finish.\n",
        ),
        _ => {}
    }
    out
}

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

    use crate::store::PullOutcome;

    fn summary(outcome: SyncOutcome) -> SyncSummary {
        SyncSummary {
            outcome,
            pull_failed: false,
            pulled: PullOutcome::default(),
            dirty: 0,
            pushed: 0,
            push_failed: 0,
        }
    }

    #[test]
    fn terminal_outcomes_render_one_line_each() {
        for (outcome, needle) in [
            (SyncOutcome::NoToken, "not connected"),
            (SyncOutcome::Expired, "token has expired"),
            (SyncOutcome::NoUserId, "which AniList account"),
            (
                SyncOutcome::PullUnauthorized,
                "pull stopped: AniList rejected",
            ),
            (
                SyncOutcome::PullRateLimited,
                "pull stopped: hit AniList's rate limit",
            ),
            (SyncOutcome::Failed, "sync failed"),
        ] {
            let text = render_sync_summary(&summary(outcome.clone()));
            assert!(text.contains(needle), "{outcome:?}: {text}");
            assert_eq!(text.lines().count(), 1, "{outcome:?}: {text}");
        }
    }

    #[test]
    fn clean_run_says_up_to_date_on_both_sides() {
        let text = render_sync_summary(&summary(SyncOutcome::Completed));
        assert!(text.contains("nothing to pull in"), "{text}");
        assert!(text.contains("nothing to push"), "{text}");
    }

    #[test]
    fn counts_render_and_conflicts_suppress_up_to_date() {
        let mut s = summary(SyncOutcome::Completed);
        s.pulled.reconciled = 2;
        s.pulled.imported = 1;
        s.pulled.conflicts = 3;
        s.pulled.contended = 1;
        s.dirty = 5;
        s.pushed = 4;
        s.push_failed = 1;
        let text = render_sync_summary(&s);
        assert!(text.contains("pulled 2 update(s)"), "{text}");
        assert!(text.contains("imported 1 show(s)"), "{text}");
        assert!(text.contains("(3 show(s) kept your local status"), "{text}");
        assert!(text.contains("(1 show(s) changed mid-sync"), "{text}");
        assert!(text.contains("pushed 4 of 5 change(s)"), "{text}");
        assert!(text.contains("1 push(es) failed"), "{text}");
        assert!(!text.contains("up to date"), "{text}");

        let mut s = summary(SyncOutcome::Completed);
        s.pulled.conflicts = 1;
        let text = render_sync_summary(&s);
        assert!(!text.contains("nothing to pull in"), "{text}");
    }

    // zigoku suppresses up-to-date on conflicts only; imports postdate the
    // freeze, so an import-only pull must suppress it too (08 §10, ROD-472).
    #[test]
    fn an_import_only_pull_suppresses_up_to_date_by_itself() {
        let mut s = summary(SyncOutcome::Completed);
        s.pulled.imported = 3;
        let text = render_sync_summary(&s);
        assert!(text.contains("imported 3 show(s)"), "{text}");
        assert!(!text.contains("nothing to pull in"), "{text}");
    }

    #[test]
    fn unmatched_listing_caps_at_twelve_and_counts_the_rest() {
        let mut s = summary(SyncOutcome::Completed);
        s.pulled.unmatched = (1..=14).collect();
        let text = render_sync_summary(&s);
        assert!(text.contains("(14 AniList show(s)"), "{text}");
        assert_eq!(text.matches("anilist.co/anime/").count(), 12, "{text}");
        assert!(text.contains("and 2 more"), "{text}");
    }

    #[test]
    fn pull_transport_miss_reports_and_still_prints_the_push_side() {
        let mut s = summary(SyncOutcome::Completed);
        s.pull_failed = true;
        s.dirty = 2;
        s.pushed = 2;
        let text = render_sync_summary(&s);
        assert!(text.contains("pull failed"), "{text}");
        assert!(text.contains("pushed 2 of 2"), "{text}");
    }

    #[test]
    fn push_walls_append_their_stop_line() {
        let mut s = summary(SyncOutcome::Unauthorized);
        s.dirty = 2;
        let text = render_sync_summary(&s);
        assert!(text.contains("pushed 0 of 2"), "{text}");
        assert!(text.contains("rejected the token mid-run"), "{text}");

        let mut s = summary(SyncOutcome::RateLimited);
        s.dirty = 3;
        s.pushed = 1;
        let text = render_sync_summary(&s);
        assert!(text.contains("hit AniList's rate limit"), "{text}");
    }
}
