//! CLI dispatch (06 §7). Subcommand = first non-flag positional matching a
//! known name; flags may precede it; after a real query word, subcommand names
//! are search text. Exit law (06 §7.4): the query play path owns the binary's
//! only nonzero exit; every other path, usage and bad flags included, exits 0.
//! `parse` is pure over argv (without argv0); main owns process exit. The
//! sync and connect renderers live here too: pure data to text, main prints.

use std::path::Path;

use crate::domain::{self, Translation};
use crate::login::ConnectResult;
use crate::providers::SearchHit;
use crate::sync::{SyncOutcome, SyncSummary};
use crate::tui::event::{FetchClass, PlayFailure};

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

/// Whether a completed stdin read yields a usable paste line, given the byte
/// count and the buffer. Ratified looser than zigoku (08 §10): a complete line
/// is accepted even without a trailing newline, so a newline-less pipe still
/// logs in. Only an empty EOF (`n == 0`) or a cap-length read with no newline
/// (a truncated overlong paste) aborts. `cap` is the reader's byte ceiling.
pub fn paste_line_usable(n: usize, line: &str, cap: u64) -> bool {
    n != 0 && (line.ends_with('\n') || (line.len() as u64) < cap)
}

/// Login outcome line (zigoku login/login_loopback wording, punctuation ours).
/// `paste` picks the retry coaching: re-copy the fragment vs re-run the flow.
/// BadState/Canceled never reach the CLI (serve waits through bad states, and
/// Ctrl-C kills the process); their arms exist to stay total.
pub fn render_connect_result(result: &ConnectResult, auth_path: &Path, paste: bool) -> String {
    match result {
        ConnectResult::Ok { user_name } => {
            // AniList-supplied name reaches a raw CLI println with no ratatui
            // backstop; strip terminal-hostile bytes before it prints.
            let name = crate::domain::strip_controls(user_name.clone());
            format!("  ✓ signed in as {name}. Saved to {}.\n", auth_path.display())
        }
        ConnectResult::NoToken if paste => {
            "  ✗ couldn't find an access_token in that; aborted.\n".into()
        }
        ConnectResult::NoToken => "  ✗ the redirect carried no access_token.\n".into(),
        ConnectResult::Rejected if paste => {
            "  ✗ AniList rejected the token (invalid or expired); re-copy the whole fragment and retry.\n"
                .into()
        }
        ConnectResult::Rejected => {
            "  ✗ AniList rejected the token (invalid or expired); re-run to retry.\n".into()
        }
        ConnectResult::NetworkError if paste => {
            "  ✗ couldn't reach AniList to verify; check your connection and retry.\n".into()
        }
        ConnectResult::NetworkError => {
            "  ✗ couldn't reach AniList to verify; re-run shortly.\n".into()
        }
        ConnectResult::SaveFailed => {
            format!(
                "  ✗ verified, but couldn't write {}.\n",
                auth_path.display()
            )
        }
        ConnectResult::BadState => "  ✗ login state mismatch.\n".into(),
        ConnectResult::Canceled => "  login canceled.\n".into(),
    }
}

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

// ── play path: search + pick rendering ──────────────────────────────────────

/// Which network call failed. The Data and Unsupported classes read per stage:
/// a search miss is not a resolve miss, and the search-stage Unsupported is the
/// default-provider trap (megaplay cannot search), not a dead episode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchStage {
    Search,
    Episodes,
    Resolve,
}

/// Numbered search results, zigoku's three-branch count line: per-track when
/// the chosen track is stocked, else the catalog total, else bare. Titles are
/// provider claims with no framework backstop; strip terminal-hostile bytes
/// before they reach stdout.
pub fn render_search_hits(hits: &[SearchHit], translation: Translation) -> String {
    use std::fmt::Write;
    let mut out = format!("\n  {} result(s):\n\n", hits.len());
    for (i, h) in hits.iter().enumerate() {
        let title = domain::strip_controls(h.title.clone());
        let per_track = match translation {
            Translation::Sub => h.eps_sub,
            Translation::Dub => h.eps_dub,
        };
        if per_track > 0 {
            let _ = writeln!(
                out,
                "  {:>2}. {title}  ·  {per_track} {} eps",
                i + 1,
                translation.as_str()
            );
        } else if let Some(t) = h.total_episodes {
            let _ = writeln!(out, "  {:>2}. {title}  ·  {t} eps", i + 1);
        } else {
            let _ = writeln!(out, "  {:>2}. {title}", i + 1);
        }
    }
    out
}

/// Numbered episode list, zigoku's width-3 `ep <label>` rows. Labels are
/// provider claims with no framework backstop; strip terminal-hostile bytes
/// before they reach stdout.
pub fn render_episode_list(labels: &[String]) -> String {
    use std::fmt::Write;
    let mut out = format!("\n  {} episode(s):\n\n", labels.len());
    for (i, label) in labels.iter().enumerate() {
        let label = domain::strip_controls(label.clone());
        let _ = writeln!(out, "  {:>3}. ep {label}", i + 1);
    }
    out
}

/// One resolved pick from a numbered prompt. `Reprompt` is a blank line (retry,
/// no coaching); the two error kinds carry their own, formatted against `max`
/// in the IO loop. EOF and an overlong read are the caller's abort and never
/// reach here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickInput {
    Abort,
    Reprompt,
    NotNumber,
    OutOfRange,
    Pick(usize),
}

/// zigoku `promptChoice`, IO-free: trim, `q` aborts, blank reprompts, a
/// non-number and an out-of-`[1, max]` each reprompt, else the 0-based index.
pub fn classify_pick(line: &str, max: usize) -> PickInput {
    let t = line.trim();
    if t.is_empty() {
        return PickInput::Reprompt;
    }
    if t.eq_ignore_ascii_case("q") {
        return PickInput::Abort;
    }
    match t.parse::<usize>() {
        Ok(n) if (1..=max).contains(&n) => PickInput::Pick(n - 1),
        Ok(_) => PickInput::OutOfRange,
        Err(_) => PickInput::NotNumber,
    }
}

/// Provider-failure copy in the CLI's sentence register (the TUI's terse toast
/// rows are separate, app.rs). The search-stage Unsupported is the fresh-install
/// trap: the default preferred provider is megaplay, which cannot search, so
/// steer to the fix rather than parrot "unsupported".
pub fn fetch_error_line(stage: FetchStage, class: FetchClass, provider: &str) -> String {
    match class {
        FetchClass::Network => {
            format!("  ✗ can't reach {provider}: check your network, then try again.\n")
        }
        FetchClass::Blocked => format!(
            "  ✗ {provider} is blocking the request (403/451); a VPN may get you through.\n"
        ),
        FetchClass::Down => {
            format!("  ✗ {provider}'s servers are down (5xx); wait a bit and retry.\n")
        }
        FetchClass::Http => format!(
            "  ✗ {provider} rejected the request; the site may be down or its recipe drifted.\n"
        ),
        FetchClass::Data => match stage {
            FetchStage::Search => format!(
                "  ✗ couldn't parse {provider}'s search results; its format may have shifted.\n"
            ),
            FetchStage::Episodes => format!(
                "  ✗ couldn't read {provider}'s episode list; its format may have shifted.\n"
            ),
            FetchStage::Resolve => format!(
                "  ✗ {provider} returned an unexpected stream payload; the protocol may have shifted.\n"
            ),
        },
        FetchClass::Unsupported => match stage {
            // Reached only by a provider whose `supports_search` disagrees with
            // its `search` (the flag defaults true, 03 §3.2), since the run
            // binds on the flag. Kept total. No retry advice: Unsupported is
            // structural, so trying again can never answer.
            FetchStage::Search => {
                format!("  ✗ {provider} can't search directly; use the TUI.\n")
            }
            FetchStage::Episodes => {
                format!("  ✗ {provider} can't list episodes for this show.\n")
            }
            FetchStage::Resolve => {
                format!("  ✗ {provider} can't provide a playable stream for this episode.\n")
            }
        },
    }
}

/// Whether the inert `--quality` heads-up fires (06 §7 parity): only when a
/// non-default value was passed. No flag, or an explicit `best`, stays silent.
/// The flag is parsed but never wired to resolve; `default_quality` drives it.
pub fn quality_note_needed(quality: Option<&str>) -> bool {
    matches!(quality, Some(q) if !q.eq_ignore_ascii_case("best"))
}

/// Heads-up when the configured source was walked past because it cannot
/// search (ROD-491). `None` when nothing was overridden: no preference set, or
/// the preference is the source the run is using. Tuples are (name, display):
/// identity compares on the stable name, the copy shows the display one.
pub fn provider_override_note(asked: Option<(&str, &str)>, chosen: (&str, &str)) -> Option<String> {
    let (asked_name, asked_display) = asked?;
    (asked_name != chosen.0).then(|| {
        format!(
            "  (note: {asked_display} can't search, so this run uses {}.)",
            chosen.1
        )
    })
}

/// Play-failure copy in the CLI's sentence register. A resolve failure is a
/// fetch failure at the resolve stage, so it delegates to `fetch_error_line`;
/// the mpv/stream classes get their own copy; `Internal` (guard/proxy/wait, the
/// sabigoku-only hardening errors) reads as a safe stop, no zigoku analog.
pub fn player_failure_line(failure: PlayFailure, provider: &str) -> String {
    match failure {
        PlayFailure::MpvNotFound => {
            "  ✗ mpv isn't on your PATH; install mpv and try again.\n".into()
        }
        PlayFailure::MpvFailed => {
            "  ✗ mpv exited badly (it closed early or couldn't play the stream).\n".into()
        }
        PlayFailure::OpenFailed => {
            "  ✗ couldn't open the stream (the CDN may have blocked it); try again in a moment.\n"
                .into()
        }
        PlayFailure::Resolve(class) => fetch_error_line(FetchStage::Resolve, class, provider),
        PlayFailure::Internal => {
            "  ✗ playback couldn't start safely; try again or pick a different episode.\n".into()
        }
    }
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
    fn paste_line_accepts_a_complete_line_with_or_without_a_trailing_newline() {
        const CAP: u64 = 8192;
        // Empty EOF aborts.
        assert!(!paste_line_usable(0, "", CAP));
        // A complete line with a newline is usable.
        assert!(paste_line_usable(10, "a-url-here\n", CAP));
        // Ratified looser than zigoku: no trailing newline is still usable when
        // the read is short of the cap (a genuine EOF-terminated line).
        assert!(paste_line_usable(9, "a-url-her", CAP));
        // A cap-length read with no newline is truncation: abort.
        let maxed = "x".repeat(CAP as usize);
        assert!(!paste_line_usable(CAP as usize, &maxed, CAP));
        // A cap-length read that DID end in a newline is a real line at the edge.
        let mut edge = "x".repeat((CAP - 1) as usize);
        edge.push('\n');
        assert!(paste_line_usable(CAP as usize, &edge, CAP));
    }

    #[test]
    fn ok_result_strips_control_bytes_from_the_anilist_name() {
        let hostile = ConnectResult::Ok {
            user_name: "ro\u{1b}[31md\u{202e}".into(),
        };
        let text = render_connect_result(&hostile, Path::new("/tmp/auth.toml"), false);
        // The ESC and the bidi override (the teeth) are gone; any inert literal
        // residue is harmless without them.
        assert!(text.contains("signed in as"), "{text}");
        assert!(!text.contains('\u{1b}'), "escape leaked: {text:?}");
        assert!(!text.contains('\u{202e}'), "bidi leaked: {text:?}");
    }

    #[test]
    fn connect_results_render_one_line_each_and_paste_picks_the_coaching() {
        let path = Path::new("/tmp/auth.toml");
        let ok = ConnectResult::Ok {
            user_name: "rod".into(),
        };
        let text = render_connect_result(&ok, path, false);
        assert!(text.contains("signed in as rod"), "{text}");
        assert!(text.contains("/tmp/auth.toml"), "{text}");

        for (result, paste, needle) in [
            (
                ConnectResult::NoToken,
                true,
                "couldn't find an access_token",
            ),
            (
                ConnectResult::NoToken,
                false,
                "redirect carried no access_token",
            ),
            (ConnectResult::Rejected, true, "re-copy the whole fragment"),
            (ConnectResult::Rejected, false, "re-run to retry"),
            (ConnectResult::NetworkError, true, "check your connection"),
            (ConnectResult::NetworkError, false, "re-run shortly"),
            (
                ConnectResult::SaveFailed,
                false,
                "couldn't write /tmp/auth.toml",
            ),
        ] {
            let text = render_connect_result(&result, path, paste);
            assert!(text.contains(needle), "{result:?} paste={paste}: {text}");
            assert_eq!(text.lines().count(), 1, "{result:?}: {text}");
        }
    }

    fn hit(title: &str, eps_sub: u32, eps_dub: u32, total: Option<u32>) -> SearchHit {
        SearchHit {
            title: title.into(),
            eps_sub,
            eps_dub,
            total_episodes: total,
            ..Default::default()
        }
    }

    #[test]
    fn search_hits_pick_per_track_then_total_then_bare() {
        let hits = [
            hit("Frieren", 28, 0, Some(28)),
            hit("Cowboy Bebop", 0, 26, Some(26)),
            hit("Mystery", 0, 0, None),
        ];
        let sub = render_search_hits(&hits, Translation::Sub);
        assert!(sub.contains("3 result(s):"), "{sub}");
        // Per-track for the stocked sub track.
        assert!(sub.contains(" 1. Frieren  ·  28 sub eps"), "{sub}");
        // No sub track: falls to the catalog total, unlabeled.
        assert!(sub.contains(" 2. Cowboy Bebop  ·  26 eps"), "{sub}");
        // Neither: bare title.
        assert!(sub.contains(" 3. Mystery\n"), "{sub}");

        // Dub mode reads the dub track count.
        let dub = render_search_hits(&hits, Translation::Dub);
        assert!(dub.contains(" 2. Cowboy Bebop  ·  26 dub eps"), "{dub}");
    }

    #[test]
    fn search_hits_strip_control_bytes_from_the_title() {
        let hits = [hit("ro\u{1b}[31md\u{202e}", 1, 0, None)];
        let out = render_search_hits(&hits, Translation::Sub);
        assert!(!out.contains('\u{1b}'), "escape leaked: {out:?}");
        assert!(!out.contains('\u{202e}'), "bidi leaked: {out:?}");
    }

    #[test]
    fn episode_list_numbers_and_strips_labels() {
        let labels = vec!["1".to_string(), "2".to_string(), "OVA\u{202e}".to_string()];
        let out = render_episode_list(&labels);
        assert!(out.contains("3 episode(s):"), "{out}");
        assert!(out.contains("  1. ep 1"), "{out}");
        assert!(out.contains("  3. ep OVA"), "{out}");
        assert!(!out.contains('\u{202e}'), "bidi leaked: {out:?}");
    }

    #[test]
    fn pick_classifies_abort_reprompt_and_range() {
        assert_eq!(classify_pick("2\n", 5), PickInput::Pick(1));
        assert_eq!(classify_pick("  3 \n", 5), PickInput::Pick(2));
        assert_eq!(classify_pick("q\n", 5), PickInput::Abort);
        assert_eq!(classify_pick("Q", 5), PickInput::Abort);
        // Blank reprompts, never aborts (zigoku: empty line continues).
        assert_eq!(classify_pick("\n", 5), PickInput::Reprompt);
        assert_eq!(classify_pick("   ", 5), PickInput::Reprompt);
        assert_eq!(classify_pick("x", 5), PickInput::NotNumber);
        assert_eq!(classify_pick("0", 5), PickInput::OutOfRange);
        assert_eq!(classify_pick("6", 5), PickInput::OutOfRange);
        assert_eq!(classify_pick("5", 5), PickInput::Pick(4));
    }

    /// The three states the note distinguishes. Silence on a stock config is
    /// the one that matters: nothing was overridden, so nothing is explained.
    #[test]
    fn override_note_fires_only_when_a_preference_was_walked_past() {
        let senshi = ("senshi", "Senshi");
        assert_eq!(provider_override_note(None, senshi), None);
        assert_eq!(
            provider_override_note(Some(("senshi", "Senshi")), senshi),
            None
        );
        let note = provider_override_note(Some(("megaplay", "MegaPlay")), senshi)
            .expect("an incapable preference is explained");
        assert!(note.contains("MegaPlay"), "{note}");
        assert!(note.contains("Senshi"), "{note}");
        assert!(note.contains("can't search"), "{note}");
    }

    /// Identity is the stable name, never the display string; two sources are
    /// free to share a display name without the note misfiring.
    #[test]
    fn override_note_compares_names_not_display_strings() {
        assert_eq!(
            provider_override_note(Some(("senshi", "Same Label")), ("senshi", "Same Label")),
            None
        );
        assert!(provider_override_note(Some(("a", "Same Label")), ("b", "Same Label")).is_some());
    }

    /// No config nudge on this row: the run binds on `supports_search`, so
    /// pointing at `preferred_provider` would prescribe a fix for a state the
    /// user cannot reach. Every Unsupported stage reads as a dead operation.
    #[test]
    fn search_unsupported_no_longer_nudges_at_config() {
        for stage in [
            FetchStage::Search,
            FetchStage::Episodes,
            FetchStage::Resolve,
        ] {
            let line = fetch_error_line(stage, FetchClass::Unsupported, "megaplay");
            assert!(!line.contains("preferred_provider"), "{line}");
        }
        let line = fetch_error_line(FetchStage::Search, FetchClass::Unsupported, "megaplay");
        assert!(line.contains("can't search directly"), "{line}");
    }

    #[test]
    fn fetch_error_rows_name_the_provider_and_render_one_block() {
        for class in [
            FetchClass::Network,
            FetchClass::Blocked,
            FetchClass::Down,
            FetchClass::Http,
        ] {
            let line = fetch_error_line(FetchStage::Search, class, "senshi");
            assert!(line.contains("senshi"), "{class:?}: {line}");
            assert!(line.ends_with('\n'), "{class:?}: {line}");
        }
        // Data reads differently per stage.
        let s = fetch_error_line(FetchStage::Search, FetchClass::Data, "senshi");
        let r = fetch_error_line(FetchStage::Resolve, FetchClass::Data, "senshi");
        assert!(s.contains("search results"), "{s}");
        assert!(r.contains("stream payload"), "{r}");
    }

    #[test]
    fn quality_note_only_for_a_non_default_value() {
        assert!(!quality_note_needed(None));
        assert!(!quality_note_needed(Some("best")));
        assert!(!quality_note_needed(Some("Best")));
        assert!(quality_note_needed(Some("1080")));
    }

    #[test]
    fn player_failures_read_per_class_and_resolve_reuses_the_fetch_copy() {
        assert!(
            player_failure_line(PlayFailure::MpvNotFound, "senshi")
                .contains("mpv isn't on your PATH")
        );
        assert!(
            player_failure_line(PlayFailure::OpenFailed, "senshi")
                .contains("couldn't open the stream")
        );
        assert!(
            player_failure_line(PlayFailure::Internal, "senshi").contains("couldn't start safely")
        );
        // A resolve HTTP class routes through the resolve-stage fetch copy.
        let net = player_failure_line(PlayFailure::Resolve(FetchClass::Network), "senshi");
        assert_eq!(
            net,
            fetch_error_line(FetchStage::Resolve, FetchClass::Network, "senshi")
        );
        assert!(net.contains("senshi"), "{net}");
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
