//! One-time zigoku library import (ROD-507). Every zigoku fact lives here:
//! its paths, frozen schema, sentinel vocabulary, row mapping. store.rs only
//! lands pre-mapped rows; the rest of the app never learns zigoku existed.
//! Runs pre-TUI on plain stdout. Every failure degrades to a normal launch;
//! the offer burns only on an answered prompt, so failures retry next launch.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, IsTerminal, Read};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

use crate::domain::{self, Enrichment, ListStatus, Translation};
use crate::error::Error;
use crate::paths::Paths;
use crate::store::{LegacyEpisode, LegacyImportCounts, LegacyShow, Store};

const PROMPT_FLAG: &str = "zigoku_import_prompted";

/// zigoku froze at schema 18; an older db never ran the final binary.
const ZIGOKU_SCHEMA_VERSION: i64 = 18;

/// zigoku's pseudo-provider for shows with no stocked source. Carries user
/// state like any sibling but must never become a provider_binding row.
const SOURCE_UNBOUND: &str = "unbound";

/// Consent-line read cap; a real answer is a few bytes.
const ANSWER_CAP: u64 = 256;

/// Reprompt ceiling, same rationale as main's pick loop: a human never
/// fumbles y/n this often, a stdin flood must not spin forever.
const MAX_ANSWER_ATTEMPTS: usize = 1000;

/// Startup gate: silent return unless a zigoku db exists, the offer is still
/// open, and both stdio ends are a terminal (a piped launch must not hang on
/// the prompt). A store that won't open is the TUI's failure to report.
pub fn maybe_run(paths: &Paths) {
    let Some(zig_db) = zigoku_db_path(&|k| std::env::var(k).ok()) else {
        return;
    };
    if !zig_db.exists() {
        return;
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return;
    }
    let Ok(store) = Store::open(&paths.db_file()) else {
        return;
    };
    if !matches!(store.meta_get(PROMPT_FLAG), Ok(None)) {
        return;
    }
    run(&store, &zig_db);
}

fn run(store: &Store, zig_db: &Path) {
    let zig = match Connection::open_with_flags(zig_db, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(conn) => conn,
        Err(e) => return print_unreadable(&e),
    };
    let version: i64 = match zig.query_row("PRAGMA user_version", [], |r| r.get(0)) {
        Ok(v) => v,
        Err(e) => return print_unreadable(&e),
    };
    let total: i64 = match zig.query_row("SELECT COUNT(*) FROM anime", [], |r| r.get(0)) {
        Ok(n) => n,
        Err(e) => return print_unreadable(&e),
    };
    match gate(version, total) {
        Gate::Outdated => {
            println!("  ✗ this zigoku library is older than sabigoku can import.");
            println!("  update zigoku and run it once, then relaunch sabigoku to try again.");
            return;
        }
        Gate::Empty => return,
        Gate::Offer => {}
    }

    println!("Found an existing zigoku library.");
    println!("sabigoku can bring over your shows, watch states, and resume points.");
    println!("This is a one-time offer; decline and we won't ask again.");
    println!();
    print!("bring it over? [y/n] ");
    flush_stdout();
    let Some(yes) = read_consent(&mut std::io::stdin().lock()) else {
        // Stream ended without an answer; the offer stays open.
        return;
    };
    if !yes {
        return finish(store, outcome(None, 0));
    }

    println!("  importing your zigoku library…");
    flush_stdout();
    let (shows, unmatched) = match extract(&zig) {
        Ok(out) => out,
        Err(e) => return print_unreadable(&e),
    };
    finish(store, outcome(Some(store.import_legacy(&shows)), unmatched));
}

#[derive(Debug, PartialEq, Eq)]
enum Gate {
    Offer,
    Outdated,
    Empty,
}

/// Exact-version gate: 18 is the frozen final, older never ran the last
/// zigoku binary, newer cannot exist. An empty library gets no offer (and no
/// burned flag: rows may yet appear, zigoku still runs).
fn gate(version: i64, total: i64) -> Gate {
    if version != ZIGOKU_SCHEMA_VERSION {
        Gate::Outdated
    } else if total == 0 {
        Gate::Empty
    } else {
        Gate::Offer
    }
}

/// Lines for an answered prompt (`None` = declined) plus whether the
/// one-time offer burns. Decline and a successful import burn it; a failed
/// write leaves it open to retry next launch. Zero counts hide their lines.
fn outcome(
    import: Option<Result<LegacyImportCounts, Error>>,
    unmatched: i64,
) -> (Vec<String>, bool) {
    let Some(result) = import else {
        return (vec!["  import skipped; won't ask again.".into()], true);
    };
    match result {
        Ok(c) => {
            let mut lines = Vec::new();
            if c.shows > 0 {
                lines.push(format!("  imported {} show(s) from zigoku.", c.shows));
            }
            if c.episodes > 0 {
                lines.push(format!("  brought over {} resume point(s).", c.episodes));
            }
            if c.shows == 0 {
                lines.push("  nothing new to import.".into());
            }
            if unmatched > 0 {
                lines.push(format!(
                    "  ({unmatched} show(s) skipped: no AniList id in zigoku.)"
                ));
            }
            if c.conflicts > 0 {
                lines.push(format!(
                    "  ({} show(s) already in sabigoku; left as-is.)",
                    c.conflicts
                ));
            }
            (lines, true)
        }
        Err(e) => (
            vec![format!(
                "  ✗ import failed: couldn't update the local library ({e}); nothing was changed."
            )],
            false,
        ),
    }
}

fn finish(store: &Store, (lines, burn): (Vec<String>, bool)) {
    for line in &lines {
        println!("{line}");
    }
    if burn {
        let _ = store.meta_set(PROMPT_FLAG, "1");
    }
}

fn print_unreadable(e: &dyn std::fmt::Display) {
    println!("  ✗ couldn't read the zigoku library ({e}); import skipped.");
}

fn flush_stdout() {
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// zigoku's db location, mirrored from its paths.zig: `$XDG_DATA_HOME/zigoku`
/// else `~/.local/share/zigoku` (macOS: `~/Library/Application Support/zigoku`).
fn zigoku_db_path(var: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let var = |k: &str| var(k).filter(|v| !v.is_empty());
    let dir = if cfg!(target_os = "macos") {
        PathBuf::from(var("HOME")?).join("Library/Application Support/zigoku")
    } else {
        match var("XDG_DATA_HOME") {
            Some(x) => PathBuf::from(x).join("zigoku"),
            None => PathBuf::from(var("HOME")?).join(".local/share/zigoku"),
        }
    };
    Some(dir.join("zigoku.db"))
}

/// Explicit consent, no default: anything but y/yes or n/no re-asks, so an
/// accidental Enter cannot answer a prompt that writes to the library.
/// None = EOF, flood, or attempts exhausted without an answer.
fn read_consent(input: &mut impl BufRead) -> Option<bool> {
    for _ in 0..MAX_ANSWER_ATTEMPTS {
        let mut line = String::new();
        match (&mut *input).take(ANSWER_CAP).read_line(&mut line) {
            Ok(0) => return None,
            Ok(n) if n as u64 == ANSWER_CAP && !line.ends_with('\n') => return None,
            Ok(_) => match answer(&line) {
                Some(v) => return Some(v),
                None => {
                    print!("  y or n: ");
                    flush_stdout();
                }
            },
            Err(_) => return None,
        }
    }
    None
}

fn answer(line: &str) -> Option<bool> {
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

/// One zigoku `anime` row carrying user state (canonical_id present).
struct ZigRow {
    canonical_id: i64,
    source: String,
    source_id: String,
    title: String,
    list_status: String,
    user_rating: Option<f64>,
    notes: Option<String>,
    play_count: i64,
    progress: i64,
    added_at: i64,
    last_watched_at: Option<i64>,
    visible: bool,
}

/// Read and map everything importable. Returns the mapped shows plus the
/// count of zigoku rows with no canonical id (reported, never matched).
///
/// Tracked means history_visible: zigoku mints rows hidden from browse
/// traffic and flips them visible on engagement, and its History gates on
/// exactly that bit. A group with no visible sibling is browse residue, not
/// a tracked show; importing it would flood the library (a real db ran 1059
/// mapped rows, 54 visible). Unmatched counts visible rows only, same logic.
fn extract(zig: &Connection) -> rusqlite::Result<(Vec<LegacyShow>, i64)> {
    let unmatched: i64 = zig.query_row(
        "SELECT COUNT(*) FROM anime WHERE canonical_id IS NULL AND history_visible = 1",
        [],
        |r| r.get(0),
    )?;

    let mut seeds: HashMap<i64, Enrichment> = HashMap::new();
    let mut stmt = zig.prepare(
        "SELECT anilist_id, mal_id, title, title_english, native_name, cover_url,
                total_episodes, year, status, description, score, kind
         FROM canonical_anime",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Enrichment {
            anilist_id: r.get(0)?,
            mal_id: r.get(1)?,
            title_romaji: scrub(r.get::<_, Option<String>>(2)?.unwrap_or_default()),
            title_english: scrub_opt(r.get(3)?),
            title_native: scrub_opt(r.get(4)?),
            cover_url: scrub_opt(r.get(5)?),
            total_episodes: to_u32(r.get(6)?),
            year: to_u32(r.get(7)?),
            status: scrub_opt(r.get(8)?),
            description: scrub_opt(r.get(9)?),
            score: to_u32(r.get(10)?),
            kind: scrub_opt(r.get(11)?),
            ..Default::default()
        })
    })?;
    for e in rows {
        let e = e?;
        seeds.insert(e.anilist_id, e);
    }

    let mut stmt = zig.prepare(
        "SELECT canonical_id, source, source_id, title, list_status, user_rating,
                notes, play_count, progress, added_at, last_watched_at, history_visible
         FROM anime WHERE canonical_id IS NOT NULL ORDER BY rowid",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ZigRow {
            canonical_id: r.get(0)?,
            source: r.get(1)?,
            source_id: r.get(2)?,
            title: r.get(3)?,
            list_status: r.get(4)?,
            user_rating: r.get(5)?,
            notes: r.get(6)?,
            play_count: r.get(7)?,
            progress: r.get(8)?,
            added_at: r.get(9)?,
            last_watched_at: r.get(10)?,
            visible: r.get(11)?,
        })
    })?;

    let mut shows: BTreeMap<i64, LegacyShow> = BTreeMap::new();
    let mut visible: std::collections::HashSet<i64> = std::collections::HashSet::new();
    // Freshest-engagement sibling donates status; counters union via max.
    let mut best: HashMap<i64, (i64, i64)> = HashMap::new();
    for row in rows {
        let row = row?;
        if row.visible {
            visible.insert(row.canonical_id);
        }
        let rank = (row.last_watched_at.unwrap_or(i64::MIN), row.progress);
        let rating = row.user_rating.filter(|v| v.is_finite());
        let entry = shows.entry(row.canonical_id).or_insert_with(|| {
            let enrichment = seeds
                .get(&row.canonical_id)
                .cloned()
                .unwrap_or_else(|| Enrichment {
                    anilist_id: row.canonical_id,
                    ..Default::default()
                });
            LegacyShow {
                enrichment,
                list_status: ListStatus::parse(&row.list_status),
                user_rating: rating,
                notes: scrub_opt(row.notes.clone()),
                play_count: clamp_u32(row.play_count),
                progress: clamp_u32(row.progress),
                added_at: row.added_at,
                last_watched_at: row.last_watched_at,
                ..Default::default()
            }
        });
        if entry.enrichment.title_romaji.is_empty() {
            entry.enrichment.title_romaji = scrub(row.title.clone());
        }
        let prev = *best.entry(row.canonical_id).or_insert(rank);
        if rank >= prev {
            best.insert(row.canonical_id, rank);
            entry.list_status = ListStatus::parse(&row.list_status);
            entry.user_rating = rating.or(entry.user_rating);
            entry.notes = scrub_opt(row.notes.clone()).or_else(|| entry.notes.take());
        } else {
            entry.user_rating = entry.user_rating.or(rating);
            if entry.notes.is_none() {
                entry.notes = scrub_opt(row.notes.clone());
            }
        }
        entry.play_count = entry.play_count.max(clamp_u32(row.play_count));
        entry.progress = entry.progress.max(clamp_u32(row.progress));
        entry.added_at = entry.added_at.min(row.added_at);
        entry.last_watched_at = entry.last_watched_at.max(row.last_watched_at);
        if row.source != SOURCE_UNBOUND {
            entry
                .bindings
                .push((scrub(row.source), scrub(row.source_id)));
        }
    }
    shows.retain(|id, _| visible.contains(id));

    let mut stmt = zig.prepare(
        "SELECT a.canonical_id, a.source, ep.translation, ep.episode,
                ep.position_secs, ep.duration_secs, ep.fully_watched, ep.updated_at
         FROM episode_progress ep
         JOIN anime a ON a.source = ep.source AND a.source_id = ep.source_id
         WHERE a.canonical_id IS NOT NULL ORDER BY a.rowid",
    )?;
    // Sibling bindings can carry the same (translation, episode); freshest wins.
    let mut episodes: HashMap<(i64, Translation, String), LegacyEpisode> = HashMap::new();
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, Option<f64>>(4)?,
            r.get::<_, Option<f64>>(5)?,
            r.get::<_, bool>(6)?,
            r.get::<_, i64>(7)?,
        ))
    })?;
    for row in rows {
        let (canonical_id, source, translation, episode, pos, dur, watched, updated_at) = row?;
        let translation = match translation.as_str() {
            "sub" => Translation::Sub,
            "dub" => Translation::Dub,
            _ => continue,
        };
        let (position_secs, duration_secs) = (pos.unwrap_or(0.0), dur.unwrap_or(0.0));
        if !position_secs.is_finite() || !duration_secs.is_finite() {
            continue;
        }
        let ep = LegacyEpisode {
            translation,
            episode: scrub(episode),
            position_secs,
            duration_secs,
            fully_watched: watched,
            updated_at,
            last_provider: (source != SOURCE_UNBOUND).then(|| scrub(source)),
        };
        let key = (canonical_id, translation, ep.episode.clone());
        match episodes.entry(key) {
            std::collections::hash_map::Entry::Vacant(v) => {
                v.insert(ep);
            }
            std::collections::hash_map::Entry::Occupied(mut o) => {
                if ep.updated_at > o.get().updated_at {
                    o.insert(ep);
                }
            }
        }
    }
    for ((canonical_id, _, _), ep) in episodes {
        if let Some(show) = shows.get_mut(&canonical_id) {
            show.episodes.push(ep);
        }
    }

    let mut stmt = zig.prepare("SELECT canonical_id, provider FROM provider_pins")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    for row in rows {
        let (canonical_id, provider) = row?;
        if let Some(show) = shows.get_mut(&canonical_id) {
            show.pin = Some(scrub(provider));
        }
    }

    Ok((shows.into_values().collect(), unmatched))
}

fn scrub(s: String) -> String {
    domain::strip_controls(s)
}

fn scrub_opt(s: Option<String>) -> Option<String> {
    s.map(domain::strip_controls).filter(|v| !v.is_empty())
}

fn to_u32(v: Option<i64>) -> Option<u32> {
    v.and_then(|n| u32::try_from(n).ok())
}

fn clamp_u32(v: i64) -> u32 {
    u32::try_from(v.max(0)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// zigoku's frozen schema (083abd3, user_version 18) reduced to the
    /// tables the importer reads, columns complete.
    const ZIGOKU_DDL: &str = "
    CREATE TABLE anime (
        source          TEXT    NOT NULL,
        source_id       TEXT    NOT NULL,
        title           TEXT    NOT NULL,
        title_english   TEXT,
        mal_id          INTEGER,
        anilist_id      INTEGER,
        cover_url       TEXT,
        total_episodes  INTEGER,
        list_status     TEXT    NOT NULL DEFAULT 'planning',
        user_rating     REAL,
        notes           TEXT,
        play_count      INTEGER NOT NULL DEFAULT 0,
        progress        INTEGER NOT NULL DEFAULT 0,
        added_at        INTEGER NOT NULL,
        last_watched_at INTEGER,
        year            INTEGER,
        status          TEXT,
        description     TEXT,
        score           INTEGER,
        history_visible INTEGER NOT NULL DEFAULT 1,
        season          TEXT,
        native_name     TEXT,
        kind            TEXT,
        start_year      INTEGER,
        start_month     INTEGER,
        start_day       INTEGER,
        genres          TEXT,
        enrichment_fetched_at       INTEGER,
        enrichment_fieldset_version INTEGER,
        studios         TEXT,
        duration        INTEGER,
        source_material TEXT,
        rank            INTEGER,
        rank_type       TEXT,
        rank_year       INTEGER,
        next_airing_at      INTEGER,
        next_airing_episode INTEGER,
        country         TEXT,
        synced_status   TEXT,
        synced_progress INTEGER,
        canonical_id    INTEGER,
        PRIMARY KEY (source, source_id)
    );
    CREATE TABLE canonical_anime (
        anilist_id      INTEGER PRIMARY KEY,
        mal_id          INTEGER,
        title           TEXT,
        title_english   TEXT,
        cover_url       TEXT,
        total_episodes  INTEGER,
        year            INTEGER,
        status          TEXT,
        description     TEXT,
        score           INTEGER,
        season          TEXT,
        native_name     TEXT,
        kind            TEXT,
        start_year      INTEGER,
        start_month     INTEGER,
        start_day       INTEGER,
        genres          TEXT,
        enrichment_fetched_at       INTEGER,
        enrichment_fieldset_version INTEGER,
        studios         TEXT,
        duration        INTEGER,
        source_material TEXT,
        rank            INTEGER,
        rank_type       TEXT,
        rank_year       INTEGER,
        next_airing_at      INTEGER,
        next_airing_episode INTEGER,
        country         TEXT
    );
    CREATE TABLE episode_progress (
        source        TEXT NOT NULL,
        source_id     TEXT NOT NULL,
        translation   TEXT NOT NULL,
        episode       TEXT NOT NULL,
        position_secs REAL NOT NULL DEFAULT 0,
        duration_secs REAL NOT NULL DEFAULT 0,
        fully_watched INTEGER NOT NULL DEFAULT 0,
        updated_at    INTEGER NOT NULL,
        PRIMARY KEY (source, source_id, translation, episode)
    );
    CREATE TABLE provider_pins (
        canonical_id INTEGER PRIMARY KEY,
        provider     TEXT NOT NULL
    );
    PRAGMA user_version = 18;
    ";

    fn fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(ZIGOKU_DDL).unwrap();
        conn
    }

    fn add_canonical(conn: &Connection, id: i64, title: Option<&str>) {
        conn.execute(
            "INSERT INTO canonical_anime (anilist_id, title, cover_url, total_episodes, score)
             VALUES (?1, ?2, 'https://img/c.png', 28, 89)",
            (id, title),
        )
        .unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn add_show(
        conn: &Connection,
        source: &str,
        source_id: &str,
        canonical: Option<i64>,
        title: &str,
        status: &str,
        progress: i64,
        last_watched_at: Option<i64>,
        added_at: i64,
        play_count: i64,
        rating: Option<f64>,
        notes: Option<&str>,
    ) {
        conn.execute(
            "INSERT INTO anime (source, source_id, canonical_id, title, list_status,
                progress, last_watched_at, added_at, play_count, user_rating, notes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            (
                source,
                source_id,
                canonical,
                title,
                status,
                progress,
                last_watched_at,
                added_at,
                play_count,
                rating,
                notes,
            ),
        )
        .unwrap();
    }

    fn add_ep(
        conn: &Connection,
        source: &str,
        source_id: &str,
        translation: &str,
        episode: &str,
        position: f64,
        updated_at: i64,
    ) {
        conn.execute(
            "INSERT INTO episode_progress
                (source, source_id, translation, episode, position_secs, duration_secs, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 1440.0, ?6)",
            (
                source,
                source_id,
                translation,
                episode,
                position,
                updated_at,
            ),
        )
        .unwrap();
    }

    #[test]
    fn extract_folds_siblings_and_reports_unmatched() {
        let zig = fixture();
        add_canonical(&zig, 100, Some("Frieren"));
        add_show(
            &zig,
            "senshi",
            "77",
            Some(100),
            "frieren (seed)",
            "watching",
            5,
            Some(2000),
            100,
            2,
            None,
            None,
        );
        add_show(
            &zig,
            "unbound",
            "100",
            Some(100),
            "frieren (seed)",
            "completed",
            8,
            Some(1000),
            50,
            4,
            Some(7.5),
            Some("from the dark days"),
        );
        add_show(
            &zig,
            "senshi",
            "dark",
            None,
            "never enriched",
            "watching",
            1,
            None,
            10,
            1,
            None,
            None,
        );
        add_ep(&zig, "senshi", "77", "sub", "1", 100.0, 5);
        add_ep(&zig, "unbound", "100", "sub", "1", 50.0, 9);
        add_ep(&zig, "unbound", "100", "dub", "2", 30.0, 7);
        zig.execute(
            "INSERT INTO provider_pins (canonical_id, provider) VALUES (100, 'senshi')",
            [],
        )
        .unwrap();

        let (shows, unmatched) = extract(&zig).unwrap();
        assert_eq!(unmatched, 1);
        assert_eq!(shows.len(), 1);
        let show = &shows[0];
        assert_eq!(show.enrichment.anilist_id, 100);
        assert_eq!(show.enrichment.title_romaji, "Frieren");
        assert_eq!(show.enrichment.total_episodes, Some(28));
        // Freshest sibling (senshi, watched later) donates status; counters
        // union via max; rating and notes backfill from any sibling.
        assert_eq!(show.list_status, ListStatus::Watching);
        assert_eq!(show.progress, 8);
        assert_eq!(show.play_count, 4);
        assert_eq!(show.user_rating, Some(7.5));
        assert_eq!(show.notes.as_deref(), Some("from the dark days"));
        assert_eq!(show.added_at, 50);
        assert_eq!(show.last_watched_at, Some(2000));
        assert_eq!(
            show.bindings,
            vec![("senshi".to_string(), "77".to_string())]
        );
        assert_eq!(show.pin.as_deref(), Some("senshi"));

        let mut eps = show.episodes.clone();
        eps.sort_by(|a, b| a.episode.cmp(&b.episode));
        assert_eq!(eps.len(), 2);
        // Sibling collision on sub/1: the fresher unbound row wins, and the
        // unbound sentinel never becomes a last_provider.
        assert_eq!(eps[0].episode, "1");
        assert_eq!(eps[0].position_secs, 50.0);
        assert_eq!(eps[0].last_provider, None);
        assert_eq!(eps[1].episode, "2");
        assert_eq!(eps[1].translation, Translation::Dub);
    }

    #[test]
    fn extract_scrubs_control_text_and_falls_back_to_row_title() {
        let zig = fixture();
        add_canonical(&zig, 200, None);
        add_show(
            &zig,
            "sen\u{200B}shi",
            "8\u{7}8",
            Some(200),
            "Fallback\u{202E} Title",
            "watching",
            1,
            None,
            10,
            1,
            None,
            Some("no\u{200B}te"),
        );
        add_ep(
            &zig,
            "sen\u{200B}shi",
            "8\u{7}8",
            "sub",
            "1\u{FEFF}",
            10.0,
            1,
        );
        zig.execute(
            "INSERT INTO provider_pins (canonical_id, provider) VALUES (200, 'sen\u{200B}shi')",
            [],
        )
        .unwrap();

        let (shows, _) = extract(&zig).unwrap();
        let show = &shows[0];
        assert_eq!(show.enrichment.title_romaji, "Fallback Title");
        assert_eq!(show.notes.as_deref(), Some("note"));
        assert_eq!(show.episodes[0].episode, "1");
        // Provider-shaped text is render-bound too: bindings, last_provider,
        // and the pin all scrub like every other zigoku string.
        assert_eq!(
            show.bindings,
            vec![("senshi".to_string(), "88".to_string())]
        );
        assert_eq!(show.episodes[0].last_provider.as_deref(), Some("senshi"));
        assert_eq!(show.pin.as_deref(), Some("senshi"));
    }

    #[test]
    fn extract_skips_groups_zigoku_never_showed() {
        let zig = fixture();
        add_canonical(&zig, 400, Some("Browse Residue"));
        add_show(
            &zig,
            "senshi",
            "40",
            Some(400),
            "residue",
            "planning",
            0,
            None,
            10,
            0,
            None,
            None,
        );
        add_show(
            &zig,
            "senshi",
            "dark",
            None,
            "dark residue",
            "planning",
            0,
            None,
            10,
            0,
            None,
            None,
        );
        zig.execute("UPDATE anime SET history_visible = 0", [])
            .unwrap();
        add_canonical(&zig, 500, Some("Kept"));
        add_show(
            &zig,
            "senshi",
            "50",
            Some(500),
            "kept",
            "watching",
            2,
            Some(100),
            20,
            1,
            None,
            None,
        );
        add_show(
            &zig,
            "megaplay",
            "m50",
            Some(500),
            "kept",
            "watching",
            7,
            None,
            30,
            1,
            None,
            None,
        );
        zig.execute(
            "UPDATE anime SET history_visible = 0 WHERE source = 'megaplay'",
            [],
        )
        .unwrap();

        let (shows, unmatched) = extract(&zig).unwrap();
        // The all-hidden group and the hidden unmatched row are residue; the
        // hidden sibling of a visible group still contributes its counters.
        assert_eq!(unmatched, 0);
        assert_eq!(shows.len(), 1);
        assert_eq!(shows[0].enrichment.anilist_id, 500);
        assert_eq!(shows[0].progress, 7);
        assert_eq!(shows[0].bindings.len(), 2);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn db_path_xdg_wins_else_home_else_none() {
        let with = |pairs: &'static [(&'static str, &'static str)]| {
            zigoku_db_path(&move |k: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == k)
                    .map(|(_, v)| v.to_string())
            })
        };
        assert_eq!(
            with(&[("XDG_DATA_HOME", "/x"), ("HOME", "/h")]),
            Some(PathBuf::from("/x/zigoku/zigoku.db"))
        );
        assert_eq!(
            with(&[("XDG_DATA_HOME", ""), ("HOME", "/h")]),
            Some(PathBuf::from("/h/.local/share/zigoku/zigoku.db"))
        );
        assert_eq!(with(&[]), None);
    }

    #[test]
    fn gate_is_exact_version_and_empty_aware() {
        assert_eq!(gate(17, 5), Gate::Outdated);
        assert_eq!(gate(19, 5), Gate::Outdated);
        assert_eq!(gate(18, 0), Gate::Empty);
        assert_eq!(gate(18, 1), Gate::Offer);
    }

    #[test]
    fn outcome_burns_the_flag_on_answers_never_on_failure() {
        let (lines, burn) = outcome(None, 9);
        assert!(burn);
        assert_eq!(
            lines,
            vec!["  import skipped; won't ask again.".to_string()]
        );

        let counts = LegacyImportCounts {
            shows: 2,
            episodes: 3,
            conflicts: 1,
        };
        let (lines, burn) = outcome(Some(Ok(counts)), 4);
        assert!(burn);
        assert_eq!(
            lines,
            vec![
                "  imported 2 show(s) from zigoku.".to_string(),
                "  brought over 3 resume point(s).".to_string(),
                "  (4 show(s) skipped: no AniList id in zigoku.)".to_string(),
                "  (1 show(s) already in sabigoku; left as-is.)".to_string(),
            ]
        );

        // All-conflicts: zero counts hide their lines, the lead stays truthful.
        let all_conflicts = LegacyImportCounts {
            shows: 0,
            episodes: 0,
            conflicts: 5,
        };
        let (lines, burn) = outcome(Some(Ok(all_conflicts)), 0);
        assert!(burn);
        assert_eq!(
            lines,
            vec![
                "  nothing new to import.".to_string(),
                "  (5 show(s) already in sabigoku; left as-is.)".to_string(),
            ]
        );

        let (lines, burn) = outcome(Some(Err(Error::from(rusqlite::Error::InvalidQuery))), 0);
        assert!(!burn);
        assert!(lines[0].starts_with("  ✗ import failed"));
    }

    #[test]
    fn consent_requires_an_explicit_y_or_n() {
        for yes in ["y\n", "Y\n", "yes\n", "  YES  \n"] {
            assert_eq!(answer(yes), Some(true), "{yes:?}");
        }
        for no in ["n\n", "N\n", "no\n", "  NO  \n"] {
            assert_eq!(answer(no), Some(false), "{no:?}");
        }
        for neither in ["\n", "yeah\n", "", "y u askin\n", "ny\n"] {
            assert_eq!(answer(neither), None, "{neither:?}");
        }
    }

    #[test]
    fn consent_reprompts_past_blanks_and_none_on_eof() {
        let mut input = std::io::Cursor::new(&b"\nmaybe\nY\n"[..]);
        assert_eq!(read_consent(&mut input), Some(true));
        let mut input = std::io::Cursor::new(&b"\n\nn\n"[..]);
        assert_eq!(read_consent(&mut input), Some(false));
        // An accidental Enter alone never answers: EOF leaves it open.
        let mut input = std::io::Cursor::new(&b"\n"[..]);
        assert_eq!(read_consent(&mut input), None);
        let mut input = std::io::Cursor::new(&b""[..]);
        assert_eq!(read_consent(&mut input), None);
    }
}
