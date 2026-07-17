//! SQLite persistence on the 02 schema, PK'd by AniList key from day one.
//! Imports domain and paths ONLY: never source, providers, or tui (01 §5).
//!
//! Ownership: the UI thread owns the Store (04 model). Workers never hold the
//! connection; they post events and the tick writes. Do not make this Sync.
//!
//! Independent store (02 L3): this file never opens, migrates, or imports a
//! zigoku DB. Its ladder starts at 1 for the 02 §3.3 shape.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, ToSql, TransactionBehavior, named_params};

use crate::domain::{Date, Enrichment, ListStatus, Season, Show, is_still_airing};
use crate::error::Error;

const SCHEMA_VERSION: u32 = 1;

/// SQLITE_BUSY wait set in `open` (ROD-287 mechanism). Writer-vs-writer only
/// (WAL lets readers through). Short: real collisions are sub-20ms and the
/// writes ride the UI thread, so 250ms caps a stall. Does not cover the WAL
/// flip; `enable_wal` retries that by hand.
const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// Bounded hand-rolled retry for the two lock upgrades busy_timeout does not
/// cover: the WAL flip and the ladder's BEGIN IMMEDIATE.
const LOCK_RETRY_LIMIT: usize = 100;
const LOCK_RETRY_BACKOFF: Duration = Duration::from_millis(5);

/// Status-aware enrichment heal TTL (02 §5). Statuses here are AniList media
/// status only; legacy provider spellings do not exist in this store.
pub const ENRICH_TTL_FINISHED_SECS: i64 = 30 * 24 * 60 * 60;
pub const ENRICH_TTL_RELEASING_SECS: i64 = 24 * 60 * 60;
pub const ENRICH_TTL_DEFAULT_SECS: i64 = 7 * 24 * 60 * 60;

pub fn enrichment_ttl_secs(status: Option<&str>) -> i64 {
    match status {
        Some(s) if s.eq_ignore_ascii_case("FINISHED") => ENRICH_TTL_FINISHED_SECS,
        Some(s) if s.eq_ignore_ascii_case("RELEASING") => ENRICH_TTL_RELEASING_SECS,
        _ => ENRICH_TTL_DEFAULT_SECS,
    }
}

// The 02 §3.3 shape, final form in one migration (no evolutionary arcs).
// genres/studios are JSON string arrays (ratified with ROD-434); the Rust
// layer owns the encoding, SQL never splits them.
const MIGRATION_V1: &str = "
CREATE TABLE show (
    anilist_id                  INTEGER PRIMARY KEY,
    mal_id                      INTEGER,
    title_romaji                TEXT NOT NULL,
    title_english               TEXT,
    title_native                TEXT,
    cover_url                   TEXT,
    total_episodes              INTEGER,
    duration_minutes            INTEGER,
    year                        INTEGER,
    season                      TEXT,
    status                      TEXT,
    description                 TEXT,
    score                       INTEGER,
    kind                        TEXT,
    start_year                  INTEGER,
    start_month                 INTEGER,
    start_day                   INTEGER,
    genres                      TEXT,
    studios                     TEXT,
    source_material             TEXT,
    rank                        INTEGER,
    rank_type                   TEXT,
    rank_year                   INTEGER,
    next_airing_at              INTEGER,
    next_airing_episode         INTEGER,
    country                     TEXT,
    enrichment_fetched_at       INTEGER,
    enrichment_fieldset_version INTEGER,
    list_status                 TEXT NOT NULL DEFAULT 'planning',
    user_rating                 REAL,
    notes                       TEXT,
    play_count                  INTEGER NOT NULL DEFAULT 0,
    progress                    INTEGER NOT NULL DEFAULT 0,
    -- NULL = identity row only (bindable, probeable), NOT in the library (02 §3.7).
    library_added_at            INTEGER,
    last_watched_at             INTEGER,
    synced_status               TEXT,
    synced_progress             INTEGER
);

CREATE INDEX idx_show_mal ON show(mal_id);
CREATE INDEX idx_show_list_status ON show(list_status);
CREATE INDEX idx_show_last_watched ON show(last_watched_at DESC);

CREATE TABLE provider_binding (
    anilist_id   INTEGER NOT NULL REFERENCES show(anilist_id) ON DELETE CASCADE,
    provider     TEXT    NOT NULL,
    provider_id  TEXT    NOT NULL,
    bound_at     INTEGER NOT NULL,
    PRIMARY KEY (anilist_id, provider),
    UNIQUE (provider, provider_id)
);

CREATE TABLE episode_progress (
    anilist_id     INTEGER NOT NULL REFERENCES show(anilist_id) ON DELETE CASCADE,
    translation    TEXT    NOT NULL,
    -- Raw label (\"1\", \"1.5\", \"SP1\"), never coerced to a number (02 L1).
    episode        TEXT    NOT NULL,
    position_secs  REAL    NOT NULL DEFAULT 0,
    duration_secs  REAL    NOT NULL DEFAULT 0,
    fully_watched  INTEGER NOT NULL DEFAULT 0,
    updated_at     INTEGER NOT NULL,
    last_provider  TEXT,
    PRIMARY KEY (anilist_id, translation, episode)
);

CREATE TABLE episode_cache (
    anilist_id    INTEGER NOT NULL REFERENCES show(anilist_id) ON DELETE CASCADE,
    provider      TEXT    NOT NULL,
    translation   TEXT    NOT NULL,
    episodes_blob TEXT    NOT NULL,
    fetched_at    INTEGER NOT NULL,
    expires_at    INTEGER NOT NULL,
    PRIMARY KEY (anilist_id, provider, translation)
);

CREATE TABLE provider_pin (
    anilist_id INTEGER PRIMARY KEY REFERENCES show(anilist_id) ON DELETE CASCADE,
    provider   TEXT NOT NULL
);

CREATE TABLE provider_absence (
    anilist_id INTEGER NOT NULL REFERENCES show(anilist_id) ON DELETE CASCADE,
    provider   TEXT    NOT NULL,
    checked_at INTEGER NOT NULL,
    PRIMARY KEY (anilist_id, provider)
);

CREATE TABLE provider_route (
    anilist_id    INTEGER PRIMARY KEY REFERENCES show(anilist_id) ON DELETE CASCADE,
    resolved_pref TEXT NOT NULL
);

CREATE TABLE catalog_cache (
    anilist_id                  INTEGER PRIMARY KEY,
    mal_id                      INTEGER,
    title_romaji                TEXT NOT NULL,
    title_english               TEXT,
    title_native                TEXT,
    cover_url                   TEXT,
    total_episodes              INTEGER,
    duration_minutes            INTEGER,
    year                        INTEGER,
    season                      TEXT,
    status                      TEXT,
    description                 TEXT,
    score                       INTEGER,
    kind                        TEXT,
    start_year                  INTEGER,
    start_month                 INTEGER,
    start_day                   INTEGER,
    genres                      TEXT,
    studios                     TEXT,
    source_material             TEXT,
    rank                        INTEGER,
    rank_type                   TEXT,
    rank_year                   INTEGER,
    next_airing_at              INTEGER,
    next_airing_episode         INTEGER,
    country                     TEXT,
    fieldset_version            INTEGER NOT NULL,
    fetched_at                  INTEGER NOT NULL,
    expires_at                  INTEGER
);

CREATE INDEX idx_catalog_fetched ON catalog_cache(fetched_at DESC);

CREATE TABLE app_meta (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
);
";

/// Which enrichment column set a row was filled under (02 §5 CLONE idea,
/// renumbered for this schema). Bump when columns are added so older rows
/// heal on view instead of waiting out the full TTL.
pub const ENRICHMENT_FIELDSET_VERSION: u32 = 1;

/// The enrichment column set, in the one order every reader and writer uses.
/// `enrichment_from_row` reads these by index; keep the three in lockstep.
const ENRICH_COLS: &str = "anilist_id, mal_id, title_romaji, title_english, title_native, \
    cover_url, total_episodes, duration_minutes, year, season, status, description, score, \
    kind, start_year, start_month, start_day, genres, studios, source_material, rank, \
    rank_type, rank_year, next_airing_at, next_airing_episode, country";

const ENRICH_VALS: &str = ":anilist_id, :mal_id, :title_romaji, :title_english, :title_native, \
    :cover_url, :total_episodes, :duration_minutes, :year, :season, :status, :description, :score, \
    :kind, :start_year, :start_month, :start_day, :genres, :studios, :source_material, :rank, \
    :rank_type, :rank_year, :next_airing_at, :next_airing_episode, :country";

const SHOW_STATE_COLS: &str = "enrichment_fetched_at, enrichment_fieldset_version, list_status, \
    user_rating, notes, play_count, progress, library_added_at, last_watched_at, \
    synced_status, synced_progress";

/// A durable Browse/Discover/detail hit (02 §3.5). No user state, ever.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogHit {
    pub enrichment: Enrichment,
    pub fieldset_version: u32,
    pub fetched_at: i64,
    pub expires_at: Option<i64>,
}

#[derive(Debug)]
pub struct Store {
    // Deliberately NOT pub, no accessor: raw SQL outside this module would
    // bypass the upsert split and the set-once membership rules.
    conn: Connection,
}

impl Store {
    /// Open (creating if absent), flip WAL, enable FKs, migrate, prove
    /// writability. The path must be sabigoku's own DB file; there is no
    /// zigoku compatibility mode.
    pub fn open(path: &Path) -> Result<Store, Error> {
        let conn = Connection::open(path)?;
        Store::finish_open(conn, path)
    }

    /// Isolated blank in-memory DB for tests.
    pub fn open_memory() -> Result<Store, Error> {
        let conn = Connection::open_in_memory()?;
        Store::finish_open(conn, Path::new(":memory:"))
    }

    fn finish_open(conn: Connection, path: &Path) -> Result<Store, Error> {
        // Before the first statement, so migrate's BEGIN IMMEDIATE can wait
        // out a concurrent opener instead of failing (ROD-287 mechanism).
        conn.busy_timeout(BUSY_TIMEOUT)?;
        enable_wal(&conn)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrate(&conn)?;
        probe_writable(&conn, path)?;
        Ok(Store { conn })
    }
}

impl Store {
    /// Upsert a Browse/Discover/search hit (02 §3.5 write path). Runs on
    /// every successful AniList page; freshness columns always restamp.
    pub fn upsert_catalog_cache(
        &self,
        e: &Enrichment,
        now: i64,
        expires_at: Option<i64>,
    ) -> Result<(), Error> {
        let bind = EnrichBind::new(e);
        let mut params = enrich_params(e, &bind);
        params.push((":fieldset_version", &ENRICHMENT_FIELDSET_VERSION));
        params.push((":now", &now));
        params.push((":expires_at", &expires_at));
        let sql = format!(
            "INSERT INTO catalog_cache ({ENRICH_COLS}, fieldset_version, fetched_at, expires_at)
             VALUES ({ENRICH_VALS}, :fieldset_version, :now, :expires_at)
             ON CONFLICT(anilist_id) DO UPDATE SET
                {merge},
                total_episodes = COALESCE(excluded.total_episodes, catalog_cache.total_episodes),
                fieldset_version = excluded.fieldset_version,
                fetched_at = excluded.fetched_at,
                expires_at = excluded.expires_at",
            merge = enrichment_merge_set("excluded.", "catalog_cache.")
        );
        self.conn.execute(&sql, params.as_slice())?;
        Ok(())
    }

    pub fn get_catalog(&self, anilist_id: i64) -> Result<Option<CatalogHit>, Error> {
        let sql = format!(
            "SELECT {ENRICH_COLS}, fieldset_version, fetched_at, expires_at
             FROM catalog_cache WHERE anilist_id = ?1"
        );
        self.conn
            .query_row(&sql, [anilist_id], |row| {
                Ok(CatalogHit {
                    enrichment: enrichment_from_row(row)?,
                    fieldset_version: row.get(26)?,
                    fetched_at: row.get(27)?,
                    expires_at: row.get(28)?,
                })
            })
            .optional()
            .map_err(Error::from)
    }

    /// The watchlist-add writer (02 §3.7): mints or merges the show row and
    /// stamps membership set-once. User-state columns are absent from the
    /// conflict SET entirely; INSERT defaults cover them on mint. NOT
    /// COALESCE: on NOT NULL columns that always takes the excluded value and
    /// reintroduces the clobber bug (02 §5).
    pub fn add_to_library(&self, e: &Enrichment, now: i64) -> Result<(), Error> {
        let bind = EnrichBind::new(e);
        let mut params = enrich_params(e, &bind);
        params.push((":now", &now));
        let sql = format!(
            "INSERT INTO show ({ENRICH_COLS}, library_added_at)
             VALUES ({ENRICH_VALS}, :now)
             ON CONFLICT(anilist_id) DO UPDATE SET
                {merge},
                total_episodes = COALESCE(excluded.total_episodes, show.total_episodes),
                library_added_at = COALESCE(show.library_added_at, excluded.library_added_at)",
            merge = enrichment_merge_set("excluded.", "show.")
        );
        self.conn.execute(&sql, params.as_slice())?;
        Ok(())
    }

    /// Enrichment-only patch, UPDATE-only: enrichment never mints a show row
    /// (the 02 §3.7 mint list is binding, absence, library add). Returns
    /// whether a row was patched.
    ///
    /// `stamp_fresh` is the full-fieldset contract: only a confirmed complete
    /// AniList answer may stamp freshness or clear a stale total. A stamped
    /// answer with no total on an airing show proves the stored total was an
    /// availability snapshot, not the finale; COALESCE would pin it forever
    /// (ROD-419). Partial feeds pass false and can neither stamp nor clear.
    pub fn patch_show_enrichment(
        &self,
        e: &Enrichment,
        stamp_fresh: bool,
        now: i64,
    ) -> Result<bool, Error> {
        let clear_total =
            stamp_fresh && e.total_episodes.is_none() && is_still_airing(e.status.as_deref());
        let bind = EnrichBind::new(e);
        let mut params = enrich_params(e, &bind);
        params.push((":stamp", &stamp_fresh));
        params.push((":clear_total", &clear_total));
        params.push((":now", &now));
        params.push((":fieldset_version", &ENRICHMENT_FIELDSET_VERSION));
        let sql = format!(
            "UPDATE show SET
                {merge},
                total_episodes = CASE WHEN :clear_total THEN NULL
                    ELSE COALESCE(:total_episodes, total_episodes) END,
                enrichment_fetched_at = CASE WHEN :stamp THEN :now
                    ELSE enrichment_fetched_at END,
                enrichment_fieldset_version = CASE WHEN :stamp THEN :fieldset_version
                    ELSE enrichment_fieldset_version END
             WHERE anilist_id = :anilist_id",
            merge = enrichment_merge_set(":", "")
        );
        Ok(self.conn.execute(&sql, params.as_slice())? > 0)
    }

    /// Promote a cached card into the library (02 §3.5): straight enrichment
    /// copy plus the membership stamp; the cache row remains.
    pub fn promote_catalog_to_show(&self, anilist_id: i64, now: i64) -> Result<bool, Error> {
        let Some(hit) = self.get_catalog(anilist_id)? else {
            return Ok(false);
        };
        self.add_to_library(&hit.enrichment, now)?;
        Ok(true)
    }

    /// Record a play of the 1-based `episode_index` (02 §4b table). Always
    /// bumps play_count / last_watched_at and stamps membership set-once
    /// (callers gate on meaningful position; a partial or rewatch is
    /// engagement). progress ratchets only when completed, never lowers.
    /// Status via after_play. Unknown show: no-op.
    pub fn record_play(
        &self,
        anilist_id: i64,
        episode_index: u32,
        completed: bool,
        now: i64,
    ) -> Result<(), Error> {
        let Some(cur) = self.status_row(anilist_id)? else {
            return Ok(());
        };
        let new_progress = if completed {
            cur.progress.max(episode_index)
        } else {
            cur.progress
        };
        let new_status = cur.status.after_play(new_progress, cur.total, cur.airing);
        self.conn.execute(
            "UPDATE show SET
                play_count = play_count + 1,
                last_watched_at = :now,
                progress = :progress,
                list_status = :status,
                library_added_at = COALESCE(library_added_at, :now)
             WHERE anilist_id = :id",
            named_params! {
                ":now": now,
                ":progress": new_progress,
                ":status": new_status.as_str(),
                ":id": anilist_id,
            },
        )?;
        Ok(())
    }

    /// Manual status write (ROD-139): no play_count / last_watched_at.
    /// completed snaps progress to total only when total > 0 (total 0 must
    /// not zero progress). Stamps membership set-once. Unknown show: no-op.
    pub fn set_list_status(
        &self,
        anilist_id: i64,
        status: ListStatus,
        now: i64,
    ) -> Result<(), Error> {
        let Some(cur) = self.status_row(anilist_id)? else {
            return Ok(());
        };
        let new_progress = match (status, cur.total) {
            (ListStatus::Completed, Some(t)) if t > 0 => t,
            _ => cur.progress,
        };
        self.conn.execute(
            "UPDATE show SET
                list_status = :status,
                progress = :progress,
                library_added_at = COALESCE(library_added_at, :now)
             WHERE anilist_id = :id",
            named_params! {
                ":status": status.as_str(),
                ":progress": new_progress,
                ":now": now,
                ":id": anilist_id,
            },
        )?;
        Ok(())
    }

    /// Undo restore of the exact prior (status, progress) pair (ROD-193):
    /// no force-complete snap.
    pub fn restore_list_status(
        &self,
        anilist_id: i64,
        status: ListStatus,
        progress: u32,
        now: i64,
    ) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE show SET
                list_status = :status,
                progress = :progress,
                library_added_at = COALESCE(library_added_at, :now)
             WHERE anilist_id = :id",
            named_params! {
                ":status": status.as_str(),
                ":progress": progress,
                ":now": now,
                ":id": anilist_id,
            },
        )?;
        Ok(())
    }

    pub fn get_show(&self, anilist_id: i64) -> Result<Option<Show>, Error> {
        let sql =
            format!("SELECT {ENRICH_COLS}, {SHOW_STATE_COLS} FROM show WHERE anilist_id = ?1");
        self.conn
            .query_row(&sql, [anilist_id], show_from_row)
            .optional()
            .map_err(Error::from)
    }

    /// History = library rows, nothing else, ever (02 §3.7). One card per
    /// show by construction; no partition or dedup gymnastics. ListStatus
    /// group order is a render concern.
    pub fn list_history(&self) -> Result<Vec<Show>, Error> {
        let sql = format!(
            "SELECT {ENRICH_COLS}, {SHOW_STATE_COLS} FROM show
             WHERE library_added_at IS NOT NULL
             ORDER BY last_watched_at DESC NULLS LAST, library_added_at DESC, anilist_id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], show_from_row)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    /// Show-wide cascade from the PK: FKs take progress, caches, bindings,
    /// pins, absences, routes. Deliberately NOT zigoku's binding-scoped
    /// delete (02 §5 FIX-IN-RUST).
    pub fn delete_show(&self, anilist_id: i64) -> Result<bool, Error> {
        Ok(self
            .conn
            .execute("DELETE FROM show WHERE anilist_id = ?1", [anilist_id])?
            > 0)
    }

    fn status_row(&self, anilist_id: i64) -> Result<Option<StatusRow>, Error> {
        self.conn
            .query_row(
                "SELECT list_status, progress, total_episodes, status
                 FROM show WHERE anilist_id = ?1",
                [anilist_id],
                |row| {
                    let list_status: String = row.get(0)?;
                    let media_status: Option<String> = row.get(3)?;
                    Ok(StatusRow {
                        status: ListStatus::parse(&list_status),
                        progress: row.get(1)?,
                        total: row.get(2)?,
                        airing: is_still_airing(media_status.as_deref()),
                    })
                },
            )
            .optional()
            .map_err(Error::from)
    }
}

struct StatusRow {
    status: ListStatus,
    progress: u32,
    total: Option<u32>,
    airing: bool,
}

/// The enrichment merge SET fragment, shared by every writer so the shape
/// cannot drift between tables. Incoming-first COALESCE: fresh non-null wins,
/// incoming NULL never wipes (a re-search must not erase enrichment). The
/// cover CASE keeps an absolute URL from ever downgrading to a relative one;
/// GLOB is case-sensitive on purpose (ROD-267). total_episodes is absent
/// here: each writer owns its own total rule (ROD-419).
fn enrichment_merge_set(new: &str, old: &str) -> String {
    let cols = [
        "mal_id",
        "title_english",
        "title_native",
        "duration_minutes",
        "year",
        "season",
        "status",
        "description",
        "score",
        "kind",
        "start_year",
        "start_month",
        "start_day",
        "genres",
        "studios",
        "source_material",
        "rank",
        "rank_type",
        "rank_year",
        "next_airing_at",
        "next_airing_episode",
        "country",
    ];
    let mut set = format!("title_romaji = {new}title_romaji");
    for col in cols {
        set.push_str(&format!(", {col} = COALESCE({new}{col}, {old}{col})"));
    }
    let old_cover = format!("{old}cover_url");
    set.push_str(&format!(
        ", cover_url = CASE
            WHEN {new}cover_url GLOB 'http://*' OR {new}cover_url GLOB 'https://*' THEN {new}cover_url
            WHEN {old_cover} GLOB 'http://*' OR {old_cover} GLOB 'https://*' THEN {old_cover}
            ELSE COALESCE({new}cover_url, {old_cover})
        END"
    ));
    set
}

/// Owned SQL forms of the encoded fields; must outlive the bind slice.
struct EnrichBind {
    season: Option<&'static str>,
    genres: Option<String>,
    studios: Option<String>,
    start_year: Option<u32>,
    start_month: Option<u32>,
    start_day: Option<u32>,
}

impl EnrichBind {
    fn new(e: &Enrichment) -> EnrichBind {
        EnrichBind {
            season: e.season.map(Season::as_str),
            genres: encode_list(&e.genres),
            studios: encode_list(&e.studios),
            start_year: e.start_date.map(|d| d.year),
            start_month: e.start_date.and_then(|d| d.month),
            start_day: e.start_date.and_then(|d| d.day),
        }
    }
}

fn enrich_params<'a>(
    e: &'a Enrichment,
    bind: &'a EnrichBind,
) -> Vec<(&'static str, &'a dyn ToSql)> {
    vec![
        (":anilist_id", &e.anilist_id),
        (":mal_id", &e.mal_id),
        (":title_romaji", &e.title_romaji),
        (":title_english", &e.title_english),
        (":title_native", &e.title_native),
        (":cover_url", &e.cover_url),
        (":total_episodes", &e.total_episodes),
        (":duration_minutes", &e.duration_minutes),
        (":year", &e.year),
        (":season", &bind.season),
        (":status", &e.status),
        (":description", &e.description),
        (":score", &e.score),
        (":kind", &e.kind),
        (":start_year", &bind.start_year),
        (":start_month", &bind.start_month),
        (":start_day", &bind.start_day),
        (":genres", &bind.genres),
        (":studios", &bind.studios),
        (":source_material", &e.source_material),
        (":rank", &e.rank),
        (":rank_type", &e.rank_type),
        (":rank_year", &e.rank_year),
        (":next_airing_at", &e.next_airing_at),
        (":next_airing_episode", &e.next_airing_episode),
        (":country", &e.country),
    ]
}

/// Empty binds NULL so the merge keeps prior values: a source that stopped
/// sending lists must not wipe them (zigoku's ROD-261 rule, JSON encoding).
fn encode_list(list: &[String]) -> Option<String> {
    if list.is_empty() {
        None
    } else {
        Some(serde_json::to_string(list).expect("Vec<String> to JSON is infallible"))
    }
}

/// Display-only lists: a corrupted cell degrades to empty, never an error.
fn decode_list(cell: Option<String>) -> Vec<String> {
    cell.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Reads the ENRICH_COLS order by index; keep in lockstep with the const.
fn enrichment_from_row(row: &rusqlite::Row) -> rusqlite::Result<Enrichment> {
    let season: Option<String> = row.get(9)?;
    let start_year: Option<u32> = row.get(14)?;
    Ok(Enrichment {
        anilist_id: row.get(0)?,
        mal_id: row.get(1)?,
        title_romaji: row.get(2)?,
        title_english: row.get(3)?,
        title_native: row.get(4)?,
        cover_url: row.get(5)?,
        total_episodes: row.get(6)?,
        duration_minutes: row.get(7)?,
        year: row.get(8)?,
        season: season.as_deref().and_then(Season::parse),
        status: row.get(10)?,
        description: row.get(11)?,
        score: row.get(12)?,
        kind: row.get(13)?,
        start_date: start_year
            .map(|year| {
                Ok::<_, rusqlite::Error>(Date {
                    year,
                    month: row.get(15)?,
                    day: row.get(16)?,
                })
            })
            .transpose()?,
        genres: decode_list(row.get(17)?),
        studios: decode_list(row.get(18)?),
        source_material: row.get(19)?,
        rank: row.get(20)?,
        rank_type: row.get(21)?,
        rank_year: row.get(22)?,
        next_airing_at: row.get(23)?,
        next_airing_episode: row.get(24)?,
        country: row.get(25)?,
    })
}

fn show_from_row(row: &rusqlite::Row) -> rusqlite::Result<Show> {
    let list_status: String = row.get(28)?;
    let synced_status: Option<String> = row.get(35)?;
    Ok(Show {
        enrichment: enrichment_from_row(row)?,
        enrichment_fetched_at: row.get(26)?,
        enrichment_fieldset_version: row.get(27)?,
        list_status: ListStatus::parse(&list_status),
        user_rating: row.get(29)?,
        notes: row.get(30)?,
        play_count: row.get(31)?,
        progress: row.get(32)?,
        library_added_at: row.get(33)?,
        last_watched_at: row.get(34)?,
        synced_status: synced_status.as_deref().map(ListStatus::parse),
        synced_progress: row.get(36)?,
    })
}

/// The retryable contention pair. rusqlite's `code` is already the primary
/// code, so extended BUSY_* variants classify here too.
fn is_contended(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _)
    if matches!(
        f.code,
        rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
    ))
}

/// Switch to WAL, retrying SQLITE_BUSY by hand: busy_timeout does not cover
/// this lock upgrade (deadlock risk), and the retry is bounded so open cannot
/// hang forever. In-memory DBs report a different mode; only the flip failing
/// hard is an error.
fn enable_wal(conn: &Connection) -> Result<(), Error> {
    let mut attempt = 0;
    loop {
        // journal_mode returns a row, so this must be a query, not an execute.
        let flipped = conn.query_row("PRAGMA journal_mode = WAL", [], |row| {
            row.get::<_, String>(0)
        });
        match flipped {
            Ok(_) => return Ok(()),
            Err(e) if attempt < LOCK_RETRY_LIMIT && is_contended(&e) => {
                attempt += 1;
                std::thread::sleep(LOCK_RETRY_BACKOFF);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

/// BEGIN IMMEDIATE with the same bounded retry as the WAL flip: a peer
/// holding the ladder lock past busy_timeout must be waited out, not turned
/// into a hard open failure. `new_unchecked` only because the retry loop
/// needs to re-borrow; drop of the returned tx is still the rollback.
fn immediate_tx(conn: &Connection) -> Result<rusqlite::Transaction<'_>, Error> {
    let mut attempt = 0;
    loop {
        match rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate) {
            Ok(tx) => return Ok(tx),
            Err(e) if attempt < LOCK_RETRY_LIMIT && is_contended(&e) => {
                attempt += 1;
                std::thread::sleep(LOCK_RETRY_BACKOFF);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

fn migrate(conn: &Connection) -> Result<(), Error> {
    // Fast path: version read without a write lock (WAL).
    if user_version(conn)? == SCHEMA_VERSION {
        return verify_schema_present(conn);
    }

    // Whole ladder under one BEGIN IMMEDIATE so DDL and the version bump are
    // atomic (no half-applied state); busy_timeout waits out a concurrent
    // opener. Drop of an uncommitted tx is the rollback.
    let tx = immediate_tx(conn)?;

    // Re-read under the lock: a peer may have finished while we waited.
    let mut v = user_version(&tx)?;
    if v == SCHEMA_VERSION {
        return verify_schema_present(&tx);
    }

    if v < 1 {
        tx.execute_batch(MIGRATION_V1)?;
        v = 1;
    }

    // Real runtime check in every build mode: a strippable assert here is
    // exactly the half-applied-schema bug (02 §5).
    if v != SCHEMA_VERSION {
        return Err(Error::MigrationIncomplete {
            at: v,
            expected: SCHEMA_VERSION,
        });
    }

    // One bump at the end; the commit below lands ladder + version together.
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

/// Validated read: negative (a legal SQLite header state) is a clean error,
/// never a raw out-of-range failure, and too-new refuses before any write in
/// both migrate paths.
fn user_version(conn: &Connection) -> Result<u32, Error> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let Ok(v) = u32::try_from(v) else {
        return Err(Error::SchemaInvalid { found: v });
    };
    if v > SCHEMA_VERSION {
        return Err(Error::SchemaTooNew {
            found: v,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(v)
}

/// user_version alone proves nothing: a stamped version over missing DDL (a
/// foreign file, a backup captured mid-write) must fail here, not as a raw
/// "no such table" at some later query (ROD-434 review finding).
fn verify_schema_present(conn: &Connection) -> Result<(), Error> {
    let tables: u32 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'show'",
        [],
        |row| row.get(0),
    )?;
    if tables == 0 {
        return Err(Error::SchemaMissing {
            version: SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// SQLite silently downgrades to a read-only connection when the file denies
/// READWRITE. That must fail loud at open, not at the first user write
/// (ROD-434 review finding). Asks sqlite3_db_readonly directly: lock probes
/// lie under WAL (they go through the -shm in the still-writable directory).
fn probe_writable(conn: &Connection, path: &Path) -> Result<(), Error> {
    if conn.is_readonly(rusqlite::DatabaseName::Main)? {
        return Err(Error::ReadOnlyDb {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Fresh file-backed DB path in the OS tmpdir; wipes any prior run's
    /// main/-wal/-shm files so every test starts blank.
    fn tmp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sabigoku-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        for suffix in ["", "-wal", "-shm"] {
            let mut p = path.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
        path
    }

    #[test]
    fn open_migrates_to_current_version() {
        let store = Store::open_memory().unwrap();
        assert_eq!(user_version(&store.conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn reopen_is_idempotent() {
        let path = tmp_db("reopen.db");
        drop(Store::open(&path).unwrap());
        let store = Store::open(&path).unwrap();
        assert_eq!(user_version(&store.conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn newer_schema_is_a_hard_error_not_a_downgrade() {
        let path = tmp_db("too-new.db");
        drop(Store::open(&path).unwrap());
        let raw = Connection::open(&path).unwrap();
        raw.pragma_update(None, "user_version", 99).unwrap();
        drop(raw);
        match Store::open(&path) {
            Err(Error::SchemaTooNew {
                found: 99,
                supported: SCHEMA_VERSION,
            }) => {}
            other => panic!("expected SchemaTooNew, got {other:?}"),
        }
        // The failed open must not have touched the version. Raw pragma read:
        // the validated helper refuses 99 by design.
        let raw = Connection::open(&path).unwrap();
        let v: i64 = raw
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(v, 99);
    }

    #[test]
    fn concurrent_openers_serialize_on_the_ladder() {
        let path = tmp_db("race.db");
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || Store::open(&path).map(|_| ()))
            })
            .collect();
        for t in threads {
            t.join().unwrap().unwrap();
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(user_version(&store.conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn open_wires_wal_busy_timeout_and_foreign_keys() {
        let path = tmp_db("pragmas.db");
        let store = Store::open(&path).unwrap();
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let busy: u64 = store
            .conn
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert_eq!(busy, BUSY_TIMEOUT.as_millis() as u64);
        let fk: u32 = store
            .conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn schema_has_every_02_table() {
        let store = Store::open_memory().unwrap();
        let mut stmt = store
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        for table in [
            "app_meta",
            "catalog_cache",
            "episode_cache",
            "episode_progress",
            "provider_absence",
            "provider_binding",
            "provider_pin",
            "provider_route",
            "show",
        ] {
            assert!(names.iter().any(|n| n == table), "missing table {table}");
        }
    }

    #[test]
    fn foreign_keys_are_enforced() {
        let store = Store::open_memory().unwrap();
        let orphan = store.conn.execute(
            "INSERT INTO provider_binding (anilist_id, provider, provider_id, bound_at)
             VALUES (1, 'senshi', 'x', 0)",
            [],
        );
        assert!(
            orphan.is_err(),
            "binding without a show row must be rejected"
        );
    }

    #[test]
    fn stamped_version_without_tables_is_rejected() {
        let path = tmp_db("ghost.db");
        let raw = Connection::open(&path).unwrap();
        raw.pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();
        drop(raw);
        match Store::open(&path) {
            Err(Error::SchemaMissing {
                version: SCHEMA_VERSION,
            }) => {}
            other => panic!("expected SchemaMissing, got {other:?}"),
        }
    }

    #[test]
    fn negative_user_version_is_a_clean_error() {
        let path = tmp_db("negative.db");
        let raw = Connection::open(&path).unwrap();
        raw.pragma_update(None, "user_version", -5).unwrap();
        drop(raw);
        match Store::open(&path) {
            Err(Error::SchemaInvalid { found: -5 }) => {}
            other => panic!("expected SchemaInvalid, got {other:?}"),
        }
    }

    #[test]
    fn read_only_db_fails_loud_at_open() {
        let path = tmp_db("readonly.db");
        drop(Store::open(&path).unwrap());
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms.clone()).unwrap();
        // CAP_DAC_OVERRIDE (root CI) makes read-only files writable anyway;
        // nothing to test there.
        if std::fs::OpenOptions::new().write(true).open(&path).is_ok() {
            return;
        }
        let result = Store::open(&path);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        match result {
            Err(Error::ReadOnlyDb { path: p }) => assert_eq!(p, path),
            other => panic!("expected ReadOnlyDb, got {other:?}"),
        }
    }

    #[test]
    fn ladder_waits_out_a_slow_peer() {
        let path = tmp_db("slow-peer.db");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let peer = std::thread::spawn({
            let path = path.clone();
            move || {
                let conn = Connection::open(&path).unwrap();
                conn.execute_batch("BEGIN IMMEDIATE").unwrap();
                ready_tx.send(()).unwrap();
                // Longer than BUSY_TIMEOUT: only the retry loops get us past.
                std::thread::sleep(Duration::from_millis(400));
                conn.execute_batch("ROLLBACK").unwrap();
            }
        });
        ready_rx.recv().unwrap();
        let store = Store::open(&path).unwrap();
        assert_eq!(user_version(&store.conn).unwrap(), SCHEMA_VERSION);
        peer.join().unwrap();
    }

    fn sample(id: i64) -> Enrichment {
        Enrichment {
            anilist_id: id,
            mal_id: Some(100 + id),
            title_romaji: format!("Show {id}"),
            title_english: Some(format!("Show {id} EN")),
            title_native: Some("ショウ".into()),
            cover_url: Some("https://img/cover.png".into()),
            total_episodes: Some(12),
            duration_minutes: Some(24),
            year: Some(2024),
            season: Some(Season::Fall),
            status: Some("FINISHED".into()),
            description: Some("desc".into()),
            score: Some(83),
            kind: Some("TV".into()),
            start_date: Some(Date {
                year: 2024,
                month: Some(10),
                day: Some(5),
            }),
            genres: vec!["Action".into(), "Drama".into()],
            studios: vec!["MAPPA".into()],
            source_material: Some("MANGA".into()),
            rank: Some(42),
            rank_type: Some("POPULAR".into()),
            rank_year: Some(2024),
            next_airing_at: None,
            next_airing_episode: None,
            country: Some("JP".into()),
        }
    }

    /// A §3.7 identity row: exists, carries no membership and no enrichment.
    fn identity_row(store: &Store, id: i64) {
        store
            .conn
            .execute(
                "INSERT INTO show (anilist_id, title_romaji) VALUES (?1, 'seed')",
                [id],
            )
            .unwrap();
    }

    #[test]
    fn catalog_upsert_round_trips() {
        let store = Store::open_memory().unwrap();
        store
            .upsert_catalog_cache(&sample(1), 1000, Some(2000))
            .unwrap();
        let hit = store.get_catalog(1).unwrap().unwrap();
        assert_eq!(hit.enrichment, sample(1));
        assert_eq!(hit.fieldset_version, ENRICHMENT_FIELDSET_VERSION);
        assert_eq!(hit.fetched_at, 1000);
        assert_eq!(hit.expires_at, Some(2000));
        assert_eq!(store.get_catalog(2).unwrap(), None);
    }

    #[test]
    fn catalog_merge_null_never_wipes() {
        let store = Store::open_memory().unwrap();
        store.upsert_catalog_cache(&sample(1), 1000, None).unwrap();
        let partial = Enrichment {
            anilist_id: 1,
            title_romaji: "Show 1 v2".into(),
            ..Enrichment::default()
        };
        store.upsert_catalog_cache(&partial, 3000, None).unwrap();
        let hit = store.get_catalog(1).unwrap().unwrap();
        assert_eq!(hit.enrichment.title_romaji, "Show 1 v2");
        assert_eq!(hit.enrichment.description.as_deref(), Some("desc"));
        assert_eq!(hit.enrichment.genres.len(), 2);
        assert_eq!(hit.enrichment.total_episodes, Some(12));
        assert_eq!(hit.fetched_at, 3000);
    }

    #[test]
    fn cover_url_never_downgrades_from_absolute() {
        let store = Store::open_memory().unwrap();
        // A relative cover sticks while nothing better exists.
        let relative = Enrichment {
            anilist_id: 1,
            title_romaji: "t".into(),
            cover_url: Some("images/rel.jpg".into()),
            ..Enrichment::default()
        };
        store.upsert_catalog_cache(&relative, 100, None).unwrap();
        let cover = |s: &Store| s.get_catalog(1).unwrap().unwrap().enrichment.cover_url;
        assert_eq!(cover(&store).as_deref(), Some("images/rel.jpg"));
        // Absolute replaces relative.
        store.upsert_catalog_cache(&sample(1), 200, None).unwrap();
        assert_eq!(cover(&store).as_deref(), Some("https://img/cover.png"));
        // Relative never downgrades a stored absolute.
        store.upsert_catalog_cache(&relative, 300, None).unwrap();
        assert_eq!(cover(&store).as_deref(), Some("https://img/cover.png"));
        // Case-shifted scheme garbage neither sticks nor clobbers (GLOB is
        // case-sensitive; ROD-267).
        let shouty = Enrichment {
            cover_url: Some("HTTPS://img/shout.png".into()),
            ..relative.clone()
        };
        store.upsert_catalog_cache(&shouty, 400, None).unwrap();
        assert_eq!(cover(&store).as_deref(), Some("https://img/cover.png"));
    }

    #[test]
    fn empty_lists_keep_prior_values() {
        let store = Store::open_memory().unwrap();
        store.upsert_catalog_cache(&sample(1), 100, None).unwrap();
        let no_lists = Enrichment {
            anilist_id: 1,
            title_romaji: "t".into(),
            genres: vec![],
            studios: vec![],
            ..Enrichment::default()
        };
        store.upsert_catalog_cache(&no_lists, 200, None).unwrap();
        let e = store.get_catalog(1).unwrap().unwrap().enrichment;
        assert_eq!(e.genres, vec!["Action".to_string(), "Drama".to_string()]);
        assert_eq!(e.studios, vec!["MAPPA".to_string()]);
    }

    #[test]
    fn add_to_library_stamps_membership_once() {
        let store = Store::open_memory().unwrap();
        store.add_to_library(&sample(2), 100).unwrap();
        let show = store.get_show(2).unwrap().unwrap();
        assert_eq!(show.library_added_at, Some(100));
        assert_eq!(show.list_status, ListStatus::Planning);
        assert_eq!(show.progress, 0);
        assert_eq!(show.enrichment, sample(2));
        // Re-add later: enrichment merges, the stamp never moves (L4).
        let renamed = Enrichment {
            title_romaji: "Renamed".into(),
            ..sample(2)
        };
        store.add_to_library(&renamed, 999).unwrap();
        let show = store.get_show(2).unwrap().unwrap();
        assert_eq!(show.library_added_at, Some(100));
        assert_eq!(show.enrichment.title_romaji, "Renamed");
    }

    #[test]
    fn enrichment_patch_never_touches_user_state() {
        let store = Store::open_memory().unwrap();
        store.add_to_library(&sample(3), 100).unwrap();
        store.set_list_status(3, ListStatus::Watching, 150).unwrap();
        store.record_play(3, 5, true, 200).unwrap();
        store
            .conn
            .execute(
                "UPDATE show SET user_rating = 8.5, notes = 'peak' WHERE anilist_id = 3",
                [],
            )
            .unwrap();
        let before = store.get_show(3).unwrap().unwrap();

        let fresh = Enrichment {
            description: Some("new synopsis".into()),
            score: Some(90),
            ..sample(3)
        };
        assert!(store.patch_show_enrichment(&fresh, true, 500).unwrap());

        let after = store.get_show(3).unwrap().unwrap();
        // The 7 user-state fields are simply absent from the SET clause.
        assert_eq!(after.list_status, before.list_status);
        assert_eq!(after.user_rating, before.user_rating);
        assert_eq!(after.notes, before.notes);
        assert_eq!(after.play_count, before.play_count);
        assert_eq!(after.progress, before.progress);
        assert_eq!(after.library_added_at, before.library_added_at);
        assert_eq!(after.last_watched_at, before.last_watched_at);
        // The enrichment side did move.
        assert_eq!(
            after.enrichment.description.as_deref(),
            Some("new synopsis")
        );
        assert_eq!(after.enrichment_fetched_at, Some(500));
        assert_eq!(
            after.enrichment_fieldset_version,
            Some(ENRICHMENT_FIELDSET_VERSION)
        );
    }

    #[test]
    fn enrichment_patch_never_mints_a_row() {
        let store = Store::open_memory().unwrap();
        assert!(!store.patch_show_enrichment(&sample(99), true, 100).unwrap());
        assert_eq!(store.get_show(99).unwrap(), None);
    }

    #[test]
    fn stale_total_clears_only_on_stamped_airing_answer() {
        let store = Store::open_memory().unwrap();
        store.add_to_library(&sample(4), 100).unwrap();
        let total = |s: &Store| s.get_show(4).unwrap().unwrap().enrichment.total_episodes;
        assert_eq!(total(&store), Some(12));

        // Unstamped partial with no total: COALESCE keeps 12.
        let airing_partial = Enrichment {
            status: Some("RELEASING".into()),
            total_episodes: None,
            ..sample(4)
        };
        store
            .patch_show_enrichment(&airing_partial, false, 200)
            .unwrap();
        assert_eq!(total(&store), Some(12));

        // Stamped full answer, airing, no total: the stored total was an
        // availability snapshot; clear it (ROD-419).
        store
            .patch_show_enrichment(&airing_partial, true, 300)
            .unwrap();
        assert_eq!(total(&store), None);

        // Finale lands later.
        let finished = Enrichment {
            total_episodes: Some(13),
            ..sample(4)
        };
        store.patch_show_enrichment(&finished, true, 400).unwrap();
        assert_eq!(total(&store), Some(13));

        // Stamped FINISHED with a null total keeps the known finale.
        let finished_no_total = Enrichment {
            total_episodes: None,
            ..sample(4)
        };
        store
            .patch_show_enrichment(&finished_no_total, true, 500)
            .unwrap();
        assert_eq!(total(&store), Some(13));
    }

    #[test]
    fn record_play_engagement_and_ratchet() {
        let store = Store::open_memory().unwrap();
        store.record_play(5, 1, true, 100).unwrap();
        assert_eq!(store.get_show(5).unwrap(), None, "unknown show is a no-op");

        identity_row(&store, 5);
        store.record_play(5, 1, false, 100).unwrap();
        let show = store.get_show(5).unwrap().unwrap();
        assert_eq!(show.play_count, 1);
        assert_eq!(show.last_watched_at, Some(100));
        assert_eq!(
            show.library_added_at,
            Some(100),
            "successful play joins History"
        );
        assert_eq!(show.progress, 0, "not completed: no ratchet");
        assert_eq!(show.list_status, ListStatus::Watching);

        store.record_play(5, 7, true, 200).unwrap();
        store.record_play(5, 3, true, 300).unwrap();
        let show = store.get_show(5).unwrap().unwrap();
        assert_eq!(show.progress, 7, "ratchet never lowers");
        assert_eq!(show.play_count, 3);
        assert_eq!(show.last_watched_at, Some(300));
        assert_eq!(
            show.library_added_at,
            Some(100),
            "membership stamp never moves"
        );
    }

    #[test]
    fn record_play_status_transitions() {
        let store = Store::open_memory().unwrap();
        // Settled show at its finale: completes.
        store.add_to_library(&sample(6), 50).unwrap();
        store.record_play(6, 12, true, 100).unwrap();
        assert_eq!(
            store.get_show(6).unwrap().unwrap().list_status,
            ListStatus::Completed
        );
        // Rewatching an early episode never demotes.
        store.record_play(6, 1, false, 200).unwrap();
        assert_eq!(
            store.get_show(6).unwrap().unwrap().list_status,
            ListStatus::Completed
        );
        // Still airing at the latest aired ep: total may be aired-so-far,
        // never auto-complete (ROD-296).
        let airing = Enrichment {
            anilist_id: 7,
            status: Some("RELEASING".into()),
            ..sample(7)
        };
        store.add_to_library(&airing, 50).unwrap();
        store.record_play(7, 12, true, 100).unwrap();
        assert_eq!(
            store.get_show(7).unwrap().unwrap().list_status,
            ListStatus::Watching
        );
    }

    #[test]
    fn progress_storage_is_unclamped() {
        let store = Store::open_memory().unwrap();
        let two_parter = Enrichment {
            total_episodes: Some(2),
            ..sample(8)
        };
        store.add_to_library(&two_parter, 50).unwrap();
        store.record_play(8, 14, true, 100).unwrap();
        // The 14/2 fix is a render-time clamp; storage keeps the overshoot.
        assert_eq!(store.get_show(8).unwrap().unwrap().progress, 14);
    }

    #[test]
    fn set_list_status_snap_rules() {
        let store = Store::open_memory().unwrap();
        store.add_to_library(&sample(9), 50).unwrap();
        store.record_play(9, 3, true, 60).unwrap();
        store
            .set_list_status(9, ListStatus::Completed, 100)
            .unwrap();
        assert_eq!(
            store.get_show(9).unwrap().unwrap().progress,
            12,
            "snap to total"
        );

        // No total: completed keeps progress.
        identity_row(&store, 10);
        store
            .restore_list_status(10, ListStatus::Watching, 3, 50)
            .unwrap();
        store
            .set_list_status(10, ListStatus::Completed, 100)
            .unwrap();
        let show = store.get_show(10).unwrap().unwrap();
        assert_eq!(show.progress, 3);
        assert_eq!(
            show.library_added_at,
            Some(50),
            "status writers stamp membership"
        );

        // Total 0 must not zero progress.
        store
            .conn
            .execute(
                "INSERT INTO show (anilist_id, title_romaji, total_episodes, progress) \
                 VALUES (11, 't', 0, 4)",
                [],
            )
            .unwrap();
        store
            .set_list_status(11, ListStatus::Completed, 100)
            .unwrap();
        assert_eq!(store.get_show(11).unwrap().unwrap().progress, 4);
    }

    #[test]
    fn restore_is_the_exact_pair() {
        let store = Store::open_memory().unwrap();
        store.add_to_library(&sample(12), 50).unwrap();
        store
            .set_list_status(12, ListStatus::Completed, 60)
            .unwrap();
        assert_eq!(store.get_show(12).unwrap().unwrap().progress, 12);
        // Undo restores (watching, 7) verbatim: no snap.
        store
            .restore_list_status(12, ListStatus::Watching, 7, 70)
            .unwrap();
        let show = store.get_show(12).unwrap().unwrap();
        assert_eq!(show.list_status, ListStatus::Watching);
        assert_eq!(show.progress, 7);
    }

    #[test]
    fn history_is_library_rows_in_watch_order() {
        let store = Store::open_memory().unwrap();
        identity_row(&store, 20);
        store.add_to_library(&sample(21), 100).unwrap();
        store.record_play(21, 1, false, 500).unwrap();
        store.add_to_library(&sample(22), 200).unwrap();
        store.add_to_library(&sample(23), 300).unwrap();
        let ids: Vec<i64> = store
            .list_history()
            .unwrap()
            .iter()
            .map(|s| s.enrichment.anilist_id)
            .collect();
        // Watched first; never-watched by add recency; identity row absent.
        assert_eq!(ids, vec![21, 23, 22]);
    }

    #[test]
    fn delete_show_cascades_children() {
        let store = Store::open_memory().unwrap();
        store.add_to_library(&sample(30), 100).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO provider_binding (anilist_id, provider, provider_id, bound_at) \
                 VALUES (30, 'senshi', 'x', 0)",
                [],
            )
            .unwrap();
        store
            .conn
            .execute(
                "INSERT INTO episode_progress (anilist_id, translation, episode, updated_at) \
                 VALUES (30, 'sub', '1', 0)",
                [],
            )
            .unwrap();
        assert!(store.delete_show(30).unwrap());
        let orphans: u32 = store
            .conn
            .query_row(
                "SELECT (SELECT count(*) FROM provider_binding WHERE anilist_id = 30)
                      + (SELECT count(*) FROM episode_progress WHERE anilist_id = 30)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0);
        assert!(!store.delete_show(30).unwrap());
    }

    #[test]
    fn promote_copies_enrichment_and_stamps() {
        let store = Store::open_memory().unwrap();
        store.upsert_catalog_cache(&sample(40), 100, None).unwrap();
        assert!(store.promote_catalog_to_show(40, 400).unwrap());
        let show = store.get_show(40).unwrap().unwrap();
        assert_eq!(show.enrichment, sample(40));
        assert_eq!(show.library_added_at, Some(400));
        assert!(
            store.get_catalog(40).unwrap().is_some(),
            "cache row remains"
        );
        assert!(!store.promote_catalog_to_show(41, 400).unwrap());
    }

    #[test]
    fn enrichment_ttl_is_status_aware() {
        assert_eq!(
            enrichment_ttl_secs(Some("FINISHED")),
            ENRICH_TTL_FINISHED_SECS
        );
        assert_eq!(
            enrichment_ttl_secs(Some("finished")),
            ENRICH_TTL_FINISHED_SECS
        );
        assert_eq!(
            enrichment_ttl_secs(Some("RELEASING")),
            ENRICH_TTL_RELEASING_SECS
        );
        assert_eq!(enrichment_ttl_secs(Some("HIATUS")), ENRICH_TTL_DEFAULT_SECS);
        assert_eq!(
            enrichment_ttl_secs(Some("CANCELLED")),
            ENRICH_TTL_DEFAULT_SECS
        );
        assert_eq!(enrichment_ttl_secs(None), ENRICH_TTL_DEFAULT_SECS);
    }
}
