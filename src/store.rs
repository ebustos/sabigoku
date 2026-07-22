//! SQLite persistence on the 02 schema, PK'd by AniList key from day one.
//! Imports domain and paths ONLY: never source, providers, or tui (01 §5).
//!
//! Ownership: the UI thread owns the Store (04 model). Workers never hold the
//! connection; they post events and the tick writes. Do not make this Sync.
//!
//! Independent store (02 L3): this file never opens, migrates, or imports a
//! zigoku DB. Its ladder starts at 1 for the 02 §3.3 shape.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, ToSql, TransactionBehavior, named_params};

use crate::domain::{
    Date, Enrichment, ListStatus, Season, Show, Translation, WATCHED_RATIO, episode_label_cmp,
    is_still_airing, natural_end,
};
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

/// Ceiling on rows one pull may auto-import (06 O3). A CURRENT list this large
/// is not real use; the cap bounds a hostile/MITM response's blast radius.
const IMPORT_CAP: usize = 500;

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
        add_to_library_on(&self.conn, e, now)
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

    /// Refresh-on-view staleness (04 §10): a show row judges by its own stamp
    /// (NULL = never enriched: an import seed or a fresh mint), else the
    /// catalog_cache row by its expiry; no row anywhere is a miss. Fieldset
    /// drift re-heals without waiting out the TTL (02 §5).
    pub fn enrichment_stale(&self, anilist_id: i64, now: i64) -> Result<bool, Error> {
        if let Some(show) = self.get_show(anilist_id)? {
            let Some(fetched_at) = show.enrichment_fetched_at else {
                return Ok(true);
            };
            if show.enrichment_fieldset_version != Some(ENRICHMENT_FIELDSET_VERSION) {
                return Ok(true);
            }
            let ttl = enrichment_ttl_secs(show.enrichment.status.as_deref());
            return Ok(now >= fetched_at + ttl);
        }
        match self.get_catalog(anilist_id)? {
            Some(hit) => Ok(hit.fieldset_version != ENRICHMENT_FIELDSET_VERSION
                || hit.expires_at.is_none_or(|t| now >= t)),
            None => Ok(true),
        }
    }

    /// A confirmed-null enrich answer stamps freshness with no fields (05 §8:
    /// a true negative is an answer, never re-queried forever). UPDATE-only,
    /// like the patch: null never mints.
    pub fn stamp_enrichment_checked(&self, anilist_id: i64, now: i64) -> Result<bool, Error> {
        Ok(self.conn.execute(
            "UPDATE show SET enrichment_fetched_at = :now,
                enrichment_fieldset_version = :fieldset_version
             WHERE anilist_id = :id",
            named_params! {
                ":now": now,
                ":fieldset_version": ENRICHMENT_FIELDSET_VERSION,
                ":id": anilist_id,
            },
        )? > 0)
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
    /// engagement). `natural_end` is the >= 0.80 tier: progress ratchets only
    /// then, and never lowers. Status via after_play. Unknown show or a zero
    /// (unknown) index: no-op, membership must not ride an unknown episode.
    ///
    /// The read-compute-write runs under one BEGIN IMMEDIATE: a second
    /// process on the same file must not be able to regress the ratchet from
    /// a stale read (ROD-434 red-team finding).
    pub fn record_play(
        &self,
        anilist_id: i64,
        episode_index: u32,
        natural_end: bool,
        now: i64,
    ) -> Result<(), Error> {
        if episode_index == 0 {
            return Ok(());
        }
        let tx = immediate_tx(&self.conn)?;
        let Some(cur) = status_row(&tx, anilist_id)? else {
            return Ok(());
        };
        let new_progress = if natural_end {
            cur.progress.max(episode_index)
        } else {
            cur.progress
        };
        let new_status = cur.status.after_play(new_progress, cur.total, cur.airing);
        tx.execute(
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
        tx.commit()?;
        Ok(())
    }

    /// Atomic play-completion writer (02 §4b): the resume row and the
    /// engagement/ratchet land under ONE BEGIN IMMEDIATE, so a mid-write
    /// failure never leaves a saved resume point with the play uncounted.
    /// This is what a finished play calls; the two primitives above stay for
    /// standalone use (a 30s checkpoint is `save_progress` alone, no play bump).
    /// Callers still gate on a meaningful position (finite > 0); the 0.80/0.95
    /// thresholds are derived here from position/duration, the single authority.
    /// Non-finite floats are rejected loud (SQLite turns NaN into NULL, +Inf
    /// sails past the ratio). Unknown show or a zero index: no-op.
    #[allow(clippy::too_many_arguments)]
    pub fn record_finish(
        &self,
        anilist_id: i64,
        translation: Translation,
        episode: &str,
        episode_index: u32,
        position_secs: f64,
        duration_secs: f64,
        last_provider: Option<&str>,
        now: i64,
    ) -> Result<(), Error> {
        if !position_secs.is_finite() || !duration_secs.is_finite() {
            return Err(Error::NonFinitePosition {
                position: position_secs,
                duration: duration_secs,
            });
        }
        if episode_index == 0 {
            return Ok(());
        }
        let tx = immediate_tx(&self.conn)?;
        let Some(cur) = status_row(&tx, anilist_id)? else {
            return Ok(());
        };
        let watched = duration_secs > 0.0 && position_secs / duration_secs >= WATCHED_RATIO;
        tx.execute(
            "INSERT INTO episode_progress
                (anilist_id, translation, episode, position_secs, duration_secs,
                 fully_watched, updated_at, last_provider)
             VALUES (:id, :tt, :episode, :pos, :dur, :watched, :now, :last_provider)
             ON CONFLICT(anilist_id, translation, episode) DO UPDATE SET
                position_secs = excluded.position_secs,
                duration_secs = excluded.duration_secs,
                fully_watched = excluded.fully_watched,
                updated_at    = excluded.updated_at,
                last_provider = excluded.last_provider",
            named_params! {
                ":id": anilist_id,
                ":tt": translation.as_str(),
                ":episode": episode,
                ":pos": position_secs,
                ":dur": duration_secs,
                ":watched": watched,
                ":now": now,
                ":last_provider": last_provider,
            },
        )?;
        let new_progress = if natural_end(position_secs, duration_secs) {
            cur.progress.max(episode_index)
        } else {
            cur.progress
        };
        let new_status = cur.status.after_play(new_progress, cur.total, cur.airing);
        tx.execute(
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
        tx.commit()?;
        Ok(())
    }

    /// Manual status write (ROD-139): no play_count / last_watched_at.
    /// completed snaps progress to total only when total > 0 (total 0 must
    /// not zero progress). Stamps membership set-once. Unknown show: no-op.
    /// Same BEGIN IMMEDIATE envelope as record_play: the snap must not write
    /// through a stale read.
    pub fn set_list_status(
        &self,
        anilist_id: i64,
        status: ListStatus,
        now: i64,
    ) -> Result<(), Error> {
        let tx = immediate_tx(&self.conn)?;
        let Some(cur) = status_row(&tx, anilist_id)? else {
            return Ok(());
        };
        let new_progress = match (status, cur.total) {
            (ListStatus::Completed, Some(t)) if t > 0 => t,
            _ => cur.progress,
        };
        tx.execute(
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
        tx.commit()?;
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
}

struct StatusRow {
    status: ListStatus,
    progress: u32,
    total: Option<u32>,
    airing: bool,
}

/// Takes a &Connection (not &self) so the user-state writers can read inside
/// their own transaction.
fn status_row(conn: &Connection, anilist_id: i64) -> Result<Option<StatusRow>, Error> {
    conn.query_row(
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

/// An import seed is only worth minting if it renders as something: a blank
/// canonical title with no english/native would land a nameless library row.
/// Checks non-empty content, not just Some: a control-only title strips to
/// Some("") upstream, which would defeat a bare is_some (ROD-467 chaos pass).
fn seed_has_title(e: &Enrichment) -> bool {
    !e.title_romaji.is_empty()
        || e.title_english.as_deref().is_some_and(|s| !s.is_empty())
        || e.title_native.as_deref().is_some_and(|s| !s.is_empty())
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
    // NULLIF: a blank incoming title is absence, not a value; it must never
    // wipe a real one (red-team ROD-434; kin to zigoku's ROD-312 title guard).
    let mut set =
        format!("title_romaji = COALESCE(NULLIF({new}title_romaji, ''), {old}title_romaji)");
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

/// The watchlist-add INSERT on any connection, so an import can run it inside a
/// transaction with its status stamp (ROD-467). See [`Store::add_to_library`]
/// for the merge contract.
fn add_to_library_on(conn: &Connection, e: &Enrichment, now: i64) -> Result<(), Error> {
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
    conn.execute(&sql, params.as_slice())?;
    Ok(())
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

/// "Not stocked" negative cache TTL (ROD-347): stale reads as unchecked so
/// resolve re-probes.
pub const ABSENCE_TTL_SECS: i64 = 7 * 24 * 60 * 60;

/// Episode-list cache TTL by airing status (ROD-68 shape, AniList statuses
/// only, like `enrichment_ttl_secs`).
pub const EP_CACHE_TTL_FINISHED_SECS: i64 = 7 * 24 * 60 * 60;
pub const EP_CACHE_TTL_RELEASING_SECS: i64 = 6 * 60 * 60;
pub const EP_CACHE_TTL_DEFAULT_SECS: i64 = 24 * 60 * 60;

pub fn episode_cache_ttl_secs(status: Option<&str>) -> i64 {
    match status {
        Some(s) if s.eq_ignore_ascii_case("FINISHED") => EP_CACHE_TTL_FINISHED_SECS,
        Some(s) if s.eq_ignore_ascii_case("RELEASING") => EP_CACHE_TTL_RELEASING_SECS,
        _ => EP_CACHE_TTL_DEFAULT_SECS,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Binding {
    pub provider: String,
    pub provider_id: String,
    pub bound_at: i64,
}

/// Detail-rail availability (ROD-348): bound wins even if a negative coexists
/// (external edit); bind clears negatives on mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderAvailability {
    Unchecked,
    Bound,
    Absent,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resume {
    pub position_secs: f64,
    pub duration_secs: f64,
    pub fully_watched: bool,
}

impl Resume {
    /// The 03 §6.3.1 resume start rule: restart at 0 when fully watched, past
    /// natural end, or the saved position is unusable; else back up by
    /// `resume_offset_sec`, saturating at 0. zigoku parity: an unknown (0)
    /// duration resumes, only a real ratio restarts.
    pub fn start_secs(&self, resume_offset_sec: u32) -> f64 {
        if self.fully_watched
            || natural_end(self.position_secs, self.duration_secs)
            || !self.position_secs.is_finite()
            || self.position_secs <= 0.0
        {
            return 0.0;
        }
        (self.position_secs - f64::from(resume_offset_sec)).max(0.0)
    }
}

/// One push-work row for AniList list sync (ROD-284 shape on the show PK).
#[derive(Debug, Clone, PartialEq)]
pub struct SyncRow {
    pub anilist_id: i64,
    pub title_romaji: String,
    pub list_status: ListStatus,
    pub progress: u32,
}

/// Tally from one pull reconcile (06 §5.4, O3). `imported`: unmatched
/// WATCHING/REPEATING entries minted into the library from their seed.
/// `unmatched`: remaining remote ids with no library row, counted not imported
/// (other statuses, or WATCHING with no usable seed).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PullOutcome {
    pub reconciled: u32,
    pub conflicts: u32,
    pub contended: u32,
    pub imported: u32,
    pub unmatched: Vec<i64>,
}

impl Store {
    /// Bind a provider offering to a show. Mints the identity row when absent
    /// (binding mint is on the 02 §3.7 list; no membership). Re-bind of a
    /// provider id that pointed at another show is delete + insert (02 O5).
    /// Clears any absence so bound and absent never coexist (02 §5).
    pub fn bind_provider(
        &self,
        e: &Enrichment,
        provider: &str,
        provider_id: &str,
        now: i64,
    ) -> Result<(), Error> {
        // One transaction: a peer landing between the steal-delete and the
        // insert would otherwise surface as a raw UNIQUE(provider,
        // provider_id) failure (ROD-434 red-team finding).
        let tx = immediate_tx(&self.conn)?;
        ensure_show_row(&tx, e)?;
        tx.execute(
            "DELETE FROM provider_binding
             WHERE provider = :provider AND provider_id = :provider_id
               AND anilist_id <> :id",
            named_params! { ":provider": provider, ":provider_id": provider_id, ":id": e.anilist_id },
        )?;
        tx.execute(
            "INSERT INTO provider_binding (anilist_id, provider, provider_id, bound_at)
             VALUES (:id, :provider, :provider_id, :now)
             ON CONFLICT(anilist_id, provider) DO UPDATE SET
                provider_id = excluded.provider_id,
                bound_at = excluded.bound_at",
            named_params! { ":id": e.anilist_id, ":provider": provider, ":provider_id": provider_id, ":now": now },
        )?;
        tx.execute(
            "DELETE FROM provider_absence WHERE anilist_id = ?1 AND provider = ?2",
            (e.anilist_id, provider),
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn unbind_provider(&self, anilist_id: i64, provider: &str) -> Result<bool, Error> {
        Ok(self.conn.execute(
            "DELETE FROM provider_binding WHERE anilist_id = ?1 AND provider = ?2",
            (anilist_id, provider),
        )? > 0)
    }

    pub fn bindings_for(&self, anilist_id: i64) -> Result<Vec<Binding>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT provider, provider_id, bound_at FROM provider_binding
             WHERE anilist_id = ?1 ORDER BY provider",
        )?;
        let rows = stmt.query_map([anilist_id], |row| {
            Ok(Binding {
                provider: row.get(0)?,
                provider_id: row.get(1)?,
                bound_at: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    pub fn show_id_for_binding(
        &self,
        provider: &str,
        provider_id: &str,
    ) -> Result<Option<i64>, Error> {
        self.conn
            .query_row(
                "SELECT anilist_id FROM provider_binding
                 WHERE provider = ?1 AND provider_id = ?2",
                (provider, provider_id),
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)
    }

    /// Persist a definitive not-stocked answer (ROD-347; callers pass only a
    /// clean miss, never a transport error). Mints the identity row when
    /// absent (absence mark is on the 02 §3.7 list; no membership).
    pub fn mark_provider_absent(
        &self,
        e: &Enrichment,
        provider: &str,
        now: i64,
    ) -> Result<(), Error> {
        let tx = immediate_tx(&self.conn)?;
        ensure_show_row(&tx, e)?;
        tx.execute(
            "INSERT INTO provider_absence (anilist_id, provider, checked_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(anilist_id, provider) DO UPDATE SET checked_at = excluded.checked_at",
            (e.anilist_id, provider, now),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Fresh (within-TTL) absence. Stale reads as unchecked.
    pub fn provider_absent_fresh(
        &self,
        anilist_id: i64,
        provider: &str,
        now: i64,
    ) -> Result<bool, Error> {
        let checked_at: Option<i64> = self
            .conn
            .query_row(
                "SELECT checked_at FROM provider_absence
                 WHERE anilist_id = ?1 AND provider = ?2",
                (anilist_id, provider),
                |row| row.get(0),
            )
            .optional()?;
        Ok(checked_at.is_some_and(|t| now < t + ABSENCE_TTL_SECS))
    }

    pub fn provider_availability(
        &self,
        anilist_id: i64,
        provider: &str,
        now: i64,
    ) -> Result<ProviderAvailability, Error> {
        let bound: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM provider_binding WHERE anilist_id = ?1 AND provider = ?2",
                (anilist_id, provider),
                |row| row.get(0),
            )
            .optional()?;
        if bound.is_some() {
            return Ok(ProviderAvailability::Bound);
        }
        if self.provider_absent_fresh(anilist_id, provider, now)? {
            return Ok(ProviderAvailability::Absent);
        }
        Ok(ProviderAvailability::Unchecked)
    }

    /// Per-show forced provider (ROD-345), returned verbatim; unknown names
    /// degrade at the registry, not here. None clears.
    pub fn set_provider_pin(&self, anilist_id: i64, provider: Option<&str>) -> Result<(), Error> {
        match provider {
            Some(p) => self.conn.execute(
                "INSERT INTO provider_pin (anilist_id, provider) VALUES (?1, ?2)
                 ON CONFLICT(anilist_id) DO UPDATE SET provider = excluded.provider",
                (anilist_id, p),
            )?,
            None => self.conn.execute(
                "DELETE FROM provider_pin WHERE anilist_id = ?1",
                [anilist_id],
            )?,
        };
        Ok(())
    }

    pub fn get_provider_pin(&self, anilist_id: i64) -> Result<Option<String>, Error> {
        self.conn
            .query_row(
                "SELECT provider FROM provider_pin WHERE anilist_id = ?1",
                [anilist_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)
    }

    /// The preferred_provider this show last settled under (ROD-398), or None
    /// (never stale-resolved).
    pub fn get_route_pref(&self, anilist_id: i64) -> Result<Option<String>, Error> {
        self.conn
            .query_row(
                "SELECT resolved_pref FROM provider_route WHERE anilist_id = ?1",
                [anilist_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)
    }

    /// Mints the identity row when absent, like the absence mark: the stamp
    /// is stamp-BEFORE-fetch (03 §5.3), so it must land for shows that have
    /// never resolved. No membership (02 §3.7).
    pub fn set_route_pref(&self, e: &Enrichment, pref: &str) -> Result<(), Error> {
        let tx = immediate_tx(&self.conn)?;
        ensure_show_row(&tx, e)?;
        tx.execute(
            "INSERT INTO provider_route (anilist_id, resolved_pref) VALUES (?1, ?2)
             ON CONFLICT(anilist_id) DO UPDATE SET resolved_pref = excluded.resolved_pref",
            (e.anilist_id, pref),
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Upsert resume for (show, track, episode label). fully_watched derives
    /// from WATCHED_RATIO here and only here (02 §4b). Non-finite floats are
    /// rejected loud: SQLite silently turns a NaN REAL into NULL, and
    /// +Infinity would sail through the ratio into a permanent fully_watched
    /// (ROD-434 red-team finding).
    #[allow(clippy::too_many_arguments)]
    pub fn save_progress(
        &self,
        anilist_id: i64,
        translation: Translation,
        episode: &str,
        position_secs: f64,
        duration_secs: f64,
        last_provider: Option<&str>,
        now: i64,
    ) -> Result<(), Error> {
        if !position_secs.is_finite() || !duration_secs.is_finite() {
            return Err(Error::NonFinitePosition {
                position: position_secs,
                duration: duration_secs,
            });
        }
        let watched = duration_secs > 0.0 && position_secs / duration_secs >= WATCHED_RATIO;
        self.conn.execute(
            "INSERT INTO episode_progress
                (anilist_id, translation, episode, position_secs, duration_secs,
                 fully_watched, updated_at, last_provider)
             VALUES (:id, :tt, :episode, :pos, :dur, :watched, :now, :last_provider)
             ON CONFLICT(anilist_id, translation, episode) DO UPDATE SET
                position_secs = excluded.position_secs,
                duration_secs = excluded.duration_secs,
                fully_watched = excluded.fully_watched,
                updated_at    = excluded.updated_at,
                last_provider = excluded.last_provider",
            named_params! {
                ":id": anilist_id,
                ":tt": translation.as_str(),
                ":episode": episode,
                ":pos": position_secs,
                ":dur": duration_secs,
                ":watched": watched,
                ":now": now,
                ":last_provider": last_provider,
            },
        )?;
        Ok(())
    }

    /// Resume point or None. One row per (show, track, label) by the 02 §3.4
    /// keying; zigoku's cross-sibling union is automatic here.
    pub fn get_resume(
        &self,
        anilist_id: i64,
        translation: Translation,
        episode: &str,
    ) -> Result<Option<Resume>, Error> {
        self.conn
            .query_row(
                "SELECT position_secs, duration_secs, fully_watched FROM episode_progress
                 WHERE anilist_id = ?1 AND translation = ?2 AND episode = ?3",
                (anilist_id, translation.as_str(), episode),
                |row| {
                    Ok(Resume {
                        position_secs: row.get(0)?,
                        duration_secs: row.get(1)?,
                        fully_watched: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(Error::from)
    }

    /// Freshest partial-watch row for the show + track: the DESIGN 4.6 resume
    /// cell and its cursor override (05 §10.7). Fully-watched and unusable
    /// positions never resume.
    pub fn latest_resume(
        &self,
        anilist_id: i64,
        translation: Translation,
    ) -> Result<Option<(String, Resume)>, Error> {
        self.conn
            .query_row(
                "SELECT episode, position_secs, duration_secs, fully_watched
                 FROM episode_progress
                 WHERE anilist_id = ?1 AND translation = ?2
                   AND fully_watched = 0 AND position_secs > 0
                 ORDER BY updated_at DESC LIMIT 1",
                (anilist_id, translation.as_str()),
                |row| {
                    Ok((
                        row.get(0)?,
                        Resume {
                            position_secs: row.get(1)?,
                            duration_secs: row.get(2)?,
                            fully_watched: row.get(3)?,
                        },
                    ))
                },
            )
            .optional()
            .map_err(Error::from)
    }

    /// Recompute contract (ROD-193, verbatim intent): 1-based ordinal of the
    /// last fully-watched row among present rows in sort-key order, not a
    /// count and not the label value. Gap-watch under-counts on purpose.
    /// Translation-scoped rows; overwrites `show.progress` unconditionally
    /// (the one translation-blind counter, 02 §4b).
    pub fn recompute_progress(
        &self,
        anilist_id: i64,
        translation: Translation,
    ) -> Result<u32, Error> {
        let high_water = self.watched_high_water(anilist_id, translation)?;
        self.conn.execute(
            "UPDATE show SET progress = ?1 WHERE anilist_id = ?2",
            (high_water, anilist_id),
        )?;
        Ok(high_water)
    }

    /// Raise-only variant for reroute / fallback landing (ROD-346): a
    /// force-completed show with no progress rows must never un-complete.
    /// Returns the final stored progress (or the union when no row exists).
    pub fn raise_progress_to_union(
        &self,
        anilist_id: i64,
        translation: Translation,
    ) -> Result<u32, Error> {
        let high_water = self.watched_high_water(anilist_id, translation)?;
        self.conn.execute(
            "UPDATE show SET progress = MAX(progress, ?1) WHERE anilist_id = ?2",
            (high_water, anilist_id),
        )?;
        let stored: Option<u32> = self
            .conn
            .query_row(
                "SELECT progress FROM show WHERE anilist_id = ?1",
                [anilist_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(stored.unwrap_or(high_water))
    }

    fn watched_high_water(&self, anilist_id: i64, translation: Translation) -> Result<u32, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT episode, fully_watched FROM episode_progress
             WHERE anilist_id = ?1 AND translation = ?2",
        )?;
        let mut rows: Vec<(String, bool)> = stmt
            .query_map((anilist_id, translation.as_str()), |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<Result<_, _>>()?;
        // Stable sort: specials (all +inf) keep their incoming order.
        rows.sort_by(|a, b| episode_label_cmp(&a.0, &b.0));
        let mut high_water = 0;
        for (i, (_, watched)) in rows.iter().enumerate() {
            if *watched {
                high_water = i as u32 + 1;
            }
        }
        Ok(high_water)
    }

    /// Cache a provider episode listing. The blob is a JSON array, not
    /// zigoku's '\n' join: labels are provider-controlled and nothing
    /// enforces "no newlines", so a delimiter join lets one hostile label
    /// forge extra episodes (ROD-434 red-team finding).
    pub fn set_episode_cache(
        &self,
        anilist_id: i64,
        provider: &str,
        translation: Translation,
        episodes: &[String],
        airing_status: Option<&str>,
        now: i64,
    ) -> Result<(), Error> {
        self.conn.execute(
            "INSERT INTO episode_cache
                (anilist_id, provider, translation, episodes_blob, fetched_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(anilist_id, provider, translation) DO UPDATE SET
                episodes_blob = excluded.episodes_blob,
                fetched_at    = excluded.fetched_at,
                expires_at    = excluded.expires_at",
            (
                anilist_id,
                provider,
                translation.as_str(),
                serde_json::to_string(episodes).expect("Vec<String> to JSON is infallible"),
                now,
                now + episode_cache_ttl_secs(airing_status),
            ),
        )?;
        Ok(())
    }

    /// Cached episode list if unexpired; stale is a miss, not an error.
    pub fn get_cached_episodes(
        &self,
        anilist_id: i64,
        provider: &str,
        translation: Translation,
        now: i64,
    ) -> Result<Option<Vec<String>>, Error> {
        let row: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT episodes_blob, expires_at FROM episode_cache
                 WHERE anilist_id = ?1 AND provider = ?2 AND translation = ?3",
                (anilist_id, provider, translation.as_str()),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(blob, expires_at)| {
            if now >= expires_at {
                return None;
            }
            // A corrupt blob is a miss (refetch), never an error.
            serde_json::from_str(&blob).ok()
        }))
    }

    /// Push work-list (ROD-284): library rows whose live (status, progress)
    /// pair differs from the sync snapshot, or that were never synced.
    /// Library-only so identity rows never flood AniList planning.
    pub fn list_dirty_for_sync(&self) -> Result<Vec<SyncRow>, Error> {
        let mut stmt = self.conn.prepare(
            "SELECT anilist_id, title_romaji, list_status, progress FROM show
             WHERE library_added_at IS NOT NULL
               AND (synced_status IS NULL
                    OR synced_status <> list_status
                    OR synced_progress IS NULL
                    OR synced_progress <> progress)
             ORDER BY last_watched_at DESC NULLS LAST, library_added_at DESC, anilist_id",
        )?;
        let rows = stmt.query_map([], |row| {
            let status: String = row.get(2)?;
            Ok(SyncRow {
                anilist_id: row.get(0)?,
                title_romaji: row.get(1)?,
                list_status: ListStatus::parse(&status),
                progress: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Error::from)
    }

    /// Advance the sync snapshot after AniList accepts the pair.
    pub fn mark_synced(
        &self,
        anilist_id: i64,
        status: ListStatus,
        progress: u32,
    ) -> Result<(), Error> {
        self.conn.execute(
            "UPDATE show SET synced_status = ?1, synced_progress = ?2 WHERE anilist_id = ?3",
            (status.as_str(), progress, anilist_id),
        )?;
        Ok(())
    }

    /// Pull reconcile (06 §5.4): merge the remote list into matching library
    /// rows by `anilist_id`, clean rows included. Read-then-write per row (no
    /// wrapping transaction) so the CAS guard can catch a concurrent local edit.
    pub fn reconcile_pull(
        &self,
        remote: &[crate::anilist::RemoteEntry],
        now: i64,
    ) -> Result<PullOutcome, Error> {
        let (plan, imports, unmatched) = self.reconcile_plan(remote)?;
        self.apply_reconcile(&plan, &imports, unmatched, now)
    }

    /// Read candidates + collapsed remote list into the rows needing a write.
    /// The concurrent-edit window is between this read and [`apply_reconcile`].
    fn reconcile_plan(
        &self,
        remote: &[crate::anilist::RemoteEntry],
    ) -> Result<ReconcilePlan, Error> {
        // Collapse the flat remote list (duplicate ids across custom lists,
        // 06 §5.4) to one pair per id, keeping the highest progress. Seeds are
        // per-media, so the first non-empty one per id is kept for import (O3).
        let mut remote_map: HashMap<i64, (ListStatus, u32)> = HashMap::new();
        let mut seeds: HashMap<i64, Enrichment> = HashMap::new();
        for e in remote {
            remote_map
                .entry(e.anilist_id)
                .and_modify(|cur| {
                    if e.progress > cur.1 {
                        *cur = (e.status, e.progress);
                    }
                })
                .or_insert((e.status, e.progress));
            if let Some(seed) = &e.import_seed {
                seeds.entry(e.anilist_id).or_insert_with(|| seed.clone());
            }
        }

        struct Candidate {
            id: i64,
            local: (ListStatus, u32),
            base: Option<(ListStatus, u32)>,
        }
        let mut stmt = self.conn.prepare(
            "SELECT anilist_id, list_status, progress, synced_status, synced_progress
             FROM show WHERE library_added_at IS NOT NULL",
        )?;
        let candidates = stmt
            .query_map([], |row| {
                let status: String = row.get(1)?;
                let snap_status: Option<String> = row.get(3)?;
                let snap_progress: Option<u32> = row.get(4)?;
                let base = match (snap_status, snap_progress) {
                    (Some(s), Some(p)) => Some((ListStatus::parse(&s), p)),
                    _ => None,
                };
                Ok(Candidate {
                    id: row.get(0)?,
                    local: (ListStatus::parse(&status), row.get(2)?),
                    base,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let matched: HashSet<i64> = candidates.iter().map(|c| c.id).collect();
        let mut plan = Vec::new();
        for c in &candidates {
            let Some(&remote_pair) = remote_map.get(&c.id) else {
                continue; // library row absent from the remote list; nothing to merge
            };
            let r = reconcile(c.base, c.local, remote_pair);
            let merged = (r.status, r.progress);
            let snapshot = (r.snapshot_status, r.snapshot_progress);
            // Skip entirely when neither the local pair nor the snapshot moves.
            if merged == c.local && Some(snapshot) == c.base {
                continue;
            }
            plan.push(PlanRow {
                id: c.id,
                guard: c.local,
                merged,
                snapshot,
                conflict: r.conflict,
            });
        }

        // Partition the remote-only ids: auto-import the WATCHING slice that
        // carries a usable seed (O3); everything else is counted, not imported.
        let mut imports: Vec<ImportRow> = Vec::new();
        let mut unmatched: Vec<i64> = Vec::new();
        for (&id, &(status, progress)) in &remote_map {
            if matched.contains(&id) {
                continue;
            }
            match seeds.remove(&id) {
                Some(seed) if status == ListStatus::Watching && seed_has_title(&seed) => {
                    imports.push(ImportRow { seed, status, progress })
                }
                _ => unmatched.push(id),
            }
        }
        imports.sort_by_key(|r| r.seed.anilist_id);
        // Bound how many rows one pull can mint: a hostile or MITM'd list can
        // hold ~14k entries under the 2MB response cap. Overflow is counted as
        // unmatched, not imported (ROD-467 chaos pass).
        if imports.len() > IMPORT_CAP {
            for r in imports.drain(IMPORT_CAP..) {
                unmatched.push(r.seed.anilist_id);
            }
        }
        unmatched.sort_unstable();
        Ok((plan, imports, unmatched))
    }

    /// Apply each planned write, merged pair and snapshot in one guarded UPDATE.
    /// Zero rows changed = a concurrent edit moved the pair past the guard:
    /// count contended, leave the row (06 §5.4).
    fn apply_reconcile(
        &self,
        plan: &[PlanRow],
        imports: &[ImportRow],
        unmatched: Vec<i64>,
        now: i64,
    ) -> Result<PullOutcome, Error> {
        let mut out = PullOutcome {
            unmatched,
            ..PullOutcome::default()
        };
        // Mint each WATCHING/REPEATING seed (O3), then adopt the remote pair as
        // truth with a matching snapshot. Both statements run under ONE
        // BEGIN IMMEDIATE so no other connection sees the transient
        // Planning/0/unsynced mint (which list_dirty_for_sync would push back as
        // PLANNING, clobbering the server). add_to_library's merge is
        // COALESCE-only so a sparse seed promotes an existing identity row
        // without wiping its enrichment. The stamp is CAS-guarded on that exact
        // freshly-minted state: a concurrent add-and-edit landing between the
        // plan read and here fails the guard, so we roll back the mint and count
        // contended instead of destroying the local edit.
        for r in imports {
            let tx = immediate_tx(&self.conn)?;
            add_to_library_on(&tx, &r.seed, now)?;
            let changed = tx.execute(
                "UPDATE show SET
                    list_status = :status,
                    progress = :progress,
                    synced_status = :status,
                    synced_progress = :progress
                 WHERE anilist_id = :id
                   AND list_status = :minted
                   AND progress = 0
                   AND synced_status IS NULL",
                named_params! {
                    ":status": r.status.as_str(),
                    ":progress": r.progress,
                    ":id": r.seed.anilist_id,
                    ":minted": ListStatus::Planning.as_str(),
                },
            )?;
            if changed == 0 {
                out.contended += 1;
                continue; // drop(tx) rolls the mint back
            }
            tx.commit()?;
            out.imported += 1;
        }
        for p in plan {
            let changed = self.conn.execute(
                "UPDATE show SET
                    list_status = :status,
                    progress = :progress,
                    synced_status = :snap_status,
                    synced_progress = :snap_progress
                 WHERE anilist_id = :id
                   AND list_status = :guard_status
                   AND progress = :guard_progress",
                named_params! {
                    ":status": p.merged.0.as_str(),
                    ":progress": p.merged.1,
                    ":snap_status": p.snapshot.0.as_str(),
                    ":snap_progress": p.snapshot.1,
                    ":id": p.id,
                    ":guard_status": p.guard.0.as_str(),
                    ":guard_progress": p.guard.1,
                },
            )?;
            if changed == 0 {
                out.contended += 1;
                continue;
            }
            out.reconciled += 1;
            if p.conflict {
                out.conflicts += 1;
            }
        }
        Ok(out)
    }

    pub fn meta_get(&self, key: &str) -> Result<Option<String>, Error> {
        self.conn
            .query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(Error::from)
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<(), Error> {
        self.conn.execute(
            "INSERT INTO app_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, value),
        )?;
        Ok(())
    }
}

/// One planned reconcile write. `guard` is the pre-merge local pair the CAS
/// UPDATE is conditioned on.
struct PlanRow {
    id: i64,
    guard: (ListStatus, u32),
    merged: (ListStatus, u32),
    snapshot: (ListStatus, u32),
    conflict: bool,
}

/// One auto-imported list-only show (O3): the mint seed plus the remote pair to
/// adopt as truth. The snapshot is stamped to match so the fresh row is born
/// clean, never pushed back as a local edit.
struct ImportRow {
    seed: Enrichment,
    status: ListStatus,
    progress: u32,
}

/// reconcile_plan output: guarded updates, WATCHING seeds to mint, and the
/// remaining count-only remote ids.
type ReconcilePlan = (Vec<PlanRow>, Vec<ImportRow>, Vec<i64>);

/// Pure merge result for one row (06 §5.4). Snapshot is the raw remote pair,
/// not the merged one: that keeps a kept-local conflict dirty for the push.
struct Reconciled {
    status: ListStatus,
    progress: u32,
    snapshot_status: ListStatus,
    snapshot_progress: u32,
    conflict: bool,
}

/// The reconcile matrix (06 §5.4). `base` is the snapshot, `None` on first
/// contact (treated as Planning). Progress is `max(local, remote)`; the
/// snapshot re-baselines to the raw remote pair when remote differs from base.
fn reconcile(
    base: Option<(ListStatus, u32)>,
    local: (ListStatus, u32),
    remote: (ListStatus, u32),
) -> Reconciled {
    let eff_base = base.map_or(ListStatus::Planning, |(s, _)| s);
    let local_moved = local.0 != eff_base;
    let remote_moved = remote.0 != eff_base;
    let (status, conflict) = match (local_moved, remote_moved) {
        (false, false) => (eff_base, false),
        (false, true) => (remote.0, false),             // adopt remote
        (true, false) => (local.0, false),              // keep local
        (true, true) => (local.0, local.0 != remote.0), // keep local; conflict if divergent
    };
    let snapshot = match base {
        Some(b) if b == remote => b,
        _ => remote,
    };
    Reconciled {
        status,
        progress: local.1.max(remote.1),
        snapshot_status: snapshot.0,
        snapshot_progress: snapshot.1,
        conflict,
    }
}

/// Identity-row mint (02 §3.7): enrichment seed, no membership, and an
/// existing row is left entirely alone (mint is not a patch). Takes a
/// &Connection so bind/absence can mint inside their own transaction.
fn ensure_show_row(conn: &Connection, e: &Enrichment) -> Result<(), Error> {
    let bind = EnrichBind::new(e);
    let params = enrich_params(e, &bind);
    let sql = format!(
        "INSERT INTO show ({ENRICH_COLS}) VALUES ({ENRICH_VALS})
         ON CONFLICT(anilist_id) DO NOTHING"
    );
    conn.execute(&sql, params.as_slice())?;
    Ok(())
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
    fn record_finish_writes_resume_and_engagement_atomically() {
        let store = Store::open_memory().unwrap();
        // Unknown show is a no-op on BOTH tables (no orphan progress row).
        store
            .record_finish(
                9,
                Translation::Sub,
                "1",
                1,
                100.0,
                1000.0,
                Some("senshi"),
                50,
            )
            .unwrap();
        assert_eq!(store.get_show(9).unwrap(), None);
        assert_eq!(store.get_resume(9, Translation::Sub, "1").unwrap(), None);

        identity_row(&store, 9);
        // Partial (0.10): engagement bumps, no ratchet, resume row present.
        store
            .record_finish(
                9,
                Translation::Sub,
                "3",
                3,
                100.0,
                1000.0,
                Some("senshi"),
                100,
            )
            .unwrap();
        let show = store.get_show(9).unwrap().unwrap();
        assert_eq!(show.play_count, 1);
        assert_eq!(show.progress, 0);
        assert_eq!(show.library_added_at, Some(100));
        let resume = store.get_resume(9, Translation::Sub, "3").unwrap().unwrap();
        assert_eq!(resume.position_secs, 100.0);
        assert!(!resume.fully_watched);

        // Natural end (0.85) ratchets progress; still not fully_watched.
        store
            .record_finish(9, Translation::Sub, "5", 5, 850.0, 1000.0, None, 200)
            .unwrap();
        assert_eq!(store.get_show(9).unwrap().unwrap().progress, 5);
        assert!(
            !store
                .get_resume(9, Translation::Sub, "5")
                .unwrap()
                .unwrap()
                .fully_watched
        );

        // Watched tier (0.96) marks fully_watched.
        store
            .record_finish(9, Translation::Sub, "6", 6, 960.0, 1000.0, None, 300)
            .unwrap();
        assert!(
            store
                .get_resume(9, Translation::Sub, "6")
                .unwrap()
                .unwrap()
                .fully_watched
        );

        // A hostile +Inf position is rejected loud, and nothing is written.
        let before = store.get_show(9).unwrap().unwrap().play_count;
        let err = store
            .record_finish(
                9,
                Translation::Sub,
                "7",
                7,
                f64::INFINITY,
                1000.0,
                None,
                400,
            )
            .unwrap_err();
        assert!(matches!(err, Error::NonFinitePosition { .. }));
        assert_eq!(store.get_show(9).unwrap().unwrap().play_count, before);
        assert_eq!(store.get_resume(9, Translation::Sub, "7").unwrap(), None);
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
    fn bind_mints_identity_row_without_membership() {
        let store = Store::open_memory().unwrap();
        store
            .bind_provider(&sample(50), "senshi", "abc", 100)
            .unwrap();
        let show = store.get_show(50).unwrap().unwrap();
        assert_eq!(show.library_added_at, None, "binding never joins History");
        assert!(store.list_history().unwrap().is_empty());
        assert_eq!(
            store.bindings_for(50).unwrap(),
            vec![Binding {
                provider: "senshi".into(),
                provider_id: "abc".into(),
                bound_at: 100,
            }]
        );
        assert_eq!(
            store.show_id_for_binding("senshi", "abc").unwrap(),
            Some(50)
        );
        // Mint is not a patch: an existing row's enrichment is untouched.
        let renamed = Enrichment {
            title_romaji: "Other".into(),
            ..sample(50)
        };
        store
            .bind_provider(&renamed, "megaplay", "m1", 200)
            .unwrap();
        assert_eq!(
            store.get_show(50).unwrap().unwrap().enrichment.title_romaji,
            "Show 50"
        );
        assert_eq!(store.bindings_for(50).unwrap().len(), 2);
    }

    #[test]
    fn rebind_steals_the_provider_id_edge() {
        let store = Store::open_memory().unwrap();
        store
            .bind_provider(&sample(51), "senshi", "dup", 100)
            .unwrap();
        // Same (provider, provider_id) resolved to another show later: the
        // wrong bind dies, the edge moves (02 O5: delete + insert).
        store
            .bind_provider(&sample(52), "senshi", "dup", 200)
            .unwrap();
        assert!(store.bindings_for(51).unwrap().is_empty());
        assert_eq!(
            store.show_id_for_binding("senshi", "dup").unwrap(),
            Some(52)
        );
        // Same show re-binding a new provider id just updates the edge.
        store
            .bind_provider(&sample(52), "senshi", "dup2", 300)
            .unwrap();
        let edges = store.bindings_for(52).unwrap();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].provider_id, "dup2");
        assert!(store.unbind_provider(52, "senshi").unwrap());
        assert!(!store.unbind_provider(52, "senshi").unwrap());
    }

    #[test]
    fn absence_ttl_and_availability_precedence() {
        let store = Store::open_memory().unwrap();
        store
            .mark_provider_absent(&sample(53), "senshi", 1000)
            .unwrap();
        assert_eq!(
            store.get_show(53).unwrap().unwrap().library_added_at,
            None,
            "absence mark never joins History"
        );
        assert!(store.provider_absent_fresh(53, "senshi", 1000).unwrap());
        assert!(
            store
                .provider_absent_fresh(53, "senshi", 1000 + ABSENCE_TTL_SECS - 1)
                .unwrap()
        );
        // Stale reads as unchecked so resolve re-probes.
        assert!(
            !store
                .provider_absent_fresh(53, "senshi", 1000 + ABSENCE_TTL_SECS)
                .unwrap()
        );
        assert_eq!(
            store.provider_availability(53, "senshi", 1000).unwrap(),
            ProviderAvailability::Absent
        );
        assert_eq!(
            store.provider_availability(53, "megaplay", 1000).unwrap(),
            ProviderAvailability::Unchecked
        );
        // Bind clears the negative: bound and absent never coexist.
        store
            .bind_provider(&sample(53), "senshi", "s53", 2000)
            .unwrap();
        assert!(!store.provider_absent_fresh(53, "senshi", 2000).unwrap());
        assert_eq!(
            store.provider_availability(53, "senshi", 2000).unwrap(),
            ProviderAvailability::Bound
        );
    }

    #[test]
    fn pin_and_route_round_trip() {
        let store = Store::open_memory().unwrap();
        store
            .bind_provider(&sample(54), "senshi", "s54", 100)
            .unwrap();
        assert_eq!(store.get_provider_pin(54).unwrap(), None);
        store.set_provider_pin(54, Some("senshi")).unwrap();
        assert_eq!(
            store.get_provider_pin(54).unwrap().as_deref(),
            Some("senshi")
        );
        store.set_provider_pin(54, Some("megaplay")).unwrap();
        assert_eq!(
            store.get_provider_pin(54).unwrap().as_deref(),
            Some("megaplay")
        );
        store.set_provider_pin(54, None).unwrap();
        assert_eq!(store.get_provider_pin(54).unwrap(), None);

        assert_eq!(store.get_route_pref(54).unwrap(), None);
        store.set_route_pref(&sample(54), "senshi").unwrap();
        assert_eq!(store.get_route_pref(54).unwrap().as_deref(), Some("senshi"));
        store.set_route_pref(&sample(54), "megaplay").unwrap();
        assert_eq!(
            store.get_route_pref(54).unwrap().as_deref(),
            Some("megaplay")
        );

        // The stamp mints the identity row for a never-resolved show
        // (stamp-before-fetch must land, 03 §5.3); no membership.
        store.set_route_pref(&sample(77), "senshi").unwrap();
        assert_eq!(store.get_route_pref(77).unwrap().as_deref(), Some("senshi"));
        assert!(
            store
                .list_history()
                .unwrap()
                .iter()
                .all(|s| s.enrichment.anilist_id != 77)
        );
    }

    #[test]
    fn resume_round_trip_and_watched_ratio() {
        let store = Store::open_memory().unwrap();
        identity_row(&store, 60);
        assert_eq!(store.get_resume(60, Translation::Sub, "1").unwrap(), None);

        store
            .save_progress(60, Translation::Sub, "1", 94.9, 100.0, Some("senshi"), 500)
            .unwrap();
        let r = store
            .get_resume(60, Translation::Sub, "1")
            .unwrap()
            .unwrap();
        assert_eq!(r.position_secs, 94.9);
        assert!(!r.fully_watched, "0.949 is under WATCHED_RATIO");

        store
            .save_progress(60, Translation::Sub, "1", 95.0, 100.0, Some("senshi"), 600)
            .unwrap();
        assert!(
            store
                .get_resume(60, Translation::Sub, "1")
                .unwrap()
                .unwrap()
                .fully_watched
        );

        // Zero duration can never mark watched (division guard).
        store
            .save_progress(60, Translation::Sub, "2", 10.0, 0.0, None, 700)
            .unwrap();
        assert!(
            !store
                .get_resume(60, Translation::Sub, "2")
                .unwrap()
                .unwrap()
                .fully_watched
        );

        // Tracks never mix.
        assert_eq!(store.get_resume(60, Translation::Dub, "1").unwrap(), None);

        let last: Option<String> = store
            .conn
            .query_row(
                "SELECT last_provider FROM episode_progress
                 WHERE anilist_id = 60 AND translation = 'sub' AND episode = '1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(last.as_deref(), Some("senshi"));
    }

    #[test]
    fn latest_resume_picks_freshest_partial_watch() {
        let store = Store::open_memory().unwrap();
        identity_row(&store, 60);
        assert_eq!(store.latest_resume(60, Translation::Sub).unwrap(), None);

        // Fully watched rows and zero positions never resume.
        store
            .save_progress(60, Translation::Sub, "1", 95.0, 100.0, None, 500)
            .unwrap();
        store
            .save_progress(60, Translation::Sub, "2", 0.0, 100.0, None, 600)
            .unwrap();
        assert_eq!(store.latest_resume(60, Translation::Sub).unwrap(), None);

        store
            .save_progress(60, Translation::Sub, "3", 40.0, 100.0, None, 700)
            .unwrap();
        store
            .save_progress(60, Translation::Sub, "4", 20.0, 100.0, None, 800)
            .unwrap();
        let (label, resume) = store.latest_resume(60, Translation::Sub).unwrap().unwrap();
        assert_eq!(label, "4", "freshest updated_at wins, not deepest position");
        assert_eq!(resume.position_secs, 20.0);

        // Tracks never mix.
        assert_eq!(store.latest_resume(60, Translation::Dub).unwrap(), None);
    }

    #[test]
    fn resume_start_rule() {
        let resume = |position_secs, duration_secs, fully_watched| Resume {
            position_secs,
            duration_secs,
            fully_watched,
        };
        assert_eq!(resume(100.0, 1000.0, true).start_secs(5), 0.0);
        assert_eq!(resume(800.0, 1000.0, false).start_secs(5), 0.0);
        assert_eq!(resume(-3.0, 1000.0, false).start_secs(5), 0.0);
        assert_eq!(resume(0.0, 1000.0, false).start_secs(5), 0.0);
        assert_eq!(resume(f64::NAN, 1000.0, false).start_secs(5), 0.0);
        assert_eq!(resume(100.0, 1000.0, false).start_secs(5), 95.0);
        assert_eq!(resume(3.0, 1000.0, false).start_secs(5), 0.0);
        // Unknown duration resumes; only a real ratio restarts.
        assert_eq!(resume(500.0, 0.0, false).start_secs(5), 495.0);
    }

    #[test]
    fn recompute_is_positional_high_water() {
        let store = Store::open_memory().unwrap();
        identity_row(&store, 61);
        let watch = |ep: &str, done: bool| {
            let pos = if done { 100.0 } else { 10.0 };
            store
                .save_progress(61, Translation::Sub, ep, pos, 100.0, None, 500)
                .unwrap();
        };
        // Present rows sorted: 1, 2, 5, SP1. Last fully-watched is "5" at
        // position 3: positional, not the label value, not a count.
        watch("5", true);
        watch("1", true);
        watch("2", false);
        watch("SP1", false);
        assert_eq!(store.recompute_progress(61, Translation::Sub).unwrap(), 3);
        assert_eq!(store.get_show(61).unwrap().unwrap().progress, 3);

        // Gap-watch under-counts on purpose: only "5" watched → 1.
        let store2 = Store::open_memory().unwrap();
        identity_row(&store2, 61);
        store2
            .save_progress(61, Translation::Sub, "5", 100.0, 100.0, None, 500)
            .unwrap();
        assert_eq!(store2.recompute_progress(61, Translation::Sub).unwrap(), 1);

        // Dub rows never count toward a sub recompute.
        store2
            .save_progress(61, Translation::Dub, "1", 100.0, 100.0, None, 500)
            .unwrap();
        assert_eq!(store2.recompute_progress(61, Translation::Sub).unwrap(), 1);

        // Recompute overwrites unconditionally; no rows → 0 clears the marker.
        let store3 = Store::open_memory().unwrap();
        identity_row(&store3, 61);
        store3
            .restore_list_status(61, ListStatus::Watching, 9, 100)
            .unwrap();
        assert_eq!(store3.recompute_progress(61, Translation::Sub).unwrap(), 0);
        assert_eq!(store3.get_show(61).unwrap().unwrap().progress, 0);
    }

    #[test]
    fn raise_to_union_never_lowers() {
        let store = Store::open_memory().unwrap();
        store.add_to_library(&sample(62), 100).unwrap();
        store
            .set_list_status(62, ListStatus::Completed, 200)
            .unwrap();
        assert_eq!(store.get_show(62).unwrap().unwrap().progress, 12);
        // Landing with a single watched row (union 1) must not un-complete.
        store
            .save_progress(62, Translation::Sub, "1", 100.0, 100.0, None, 300)
            .unwrap();
        assert_eq!(
            store.raise_progress_to_union(62, Translation::Sub).unwrap(),
            12
        );
        assert_eq!(store.get_show(62).unwrap().unwrap().progress, 12);
        // No show row: nothing raised, the union is reported.
        let empty = Store::open_memory().unwrap();
        assert_eq!(
            empty
                .raise_progress_to_union(999, Translation::Sub)
                .unwrap(),
            0
        );
    }

    #[test]
    fn episode_cache_expiry_is_a_miss() {
        let store = Store::open_memory().unwrap();
        identity_row(&store, 63);
        let eps = vec!["1".to_string(), "1.5".to_string(), "SP1".to_string()];
        store
            .set_episode_cache(63, "senshi", Translation::Sub, &eps, Some("FINISHED"), 1000)
            .unwrap();
        assert_eq!(
            store
                .get_cached_episodes(63, "senshi", Translation::Sub, 1000)
                .unwrap(),
            Some(eps.clone())
        );
        assert_eq!(
            store
                .get_cached_episodes(
                    63,
                    "senshi",
                    Translation::Sub,
                    1000 + EP_CACHE_TTL_FINISHED_SECS
                )
                .unwrap(),
            None,
            "stale is a miss"
        );
        // An empty listing round-trips as empty, distinct from a miss.
        store
            .set_episode_cache(63, "senshi", Translation::Dub, &[], Some("FINISHED"), 1000)
            .unwrap();
        assert_eq!(
            store
                .get_cached_episodes(63, "senshi", Translation::Dub, 1000)
                .unwrap(),
            Some(vec![])
        );
    }

    #[test]
    fn episode_cache_ttl_is_status_aware() {
        assert_eq!(
            episode_cache_ttl_secs(Some("FINISHED")),
            EP_CACHE_TTL_FINISHED_SECS
        );
        assert_eq!(
            episode_cache_ttl_secs(Some("releasing")),
            EP_CACHE_TTL_RELEASING_SECS
        );
        assert_eq!(
            episode_cache_ttl_secs(Some("HIATUS")),
            EP_CACHE_TTL_DEFAULT_SECS
        );
        assert_eq!(episode_cache_ttl_secs(None), EP_CACHE_TTL_DEFAULT_SECS);
    }

    // ---- Pull reconcile (06 §5.4) ----

    use crate::anilist::RemoteEntry;

    fn remote(id: i64, status: ListStatus, progress: u32) -> RemoteEntry {
        RemoteEntry { anilist_id: id, status, progress, import_seed: None }
    }

    fn remote_seed(id: i64, status: ListStatus, progress: u32, romaji: &str) -> RemoteEntry {
        let import_seed = Some(Enrichment {
            anilist_id: id,
            title_romaji: romaji.to_string(),
            total_episodes: Some(12),
            ..Enrichment::default()
        });
        RemoteEntry { anilist_id: id, status, progress, import_seed }
    }

    /// Library row at a precise pair + snapshot, bypassing the auto-status snaps.
    fn lib_row(
        store: &Store,
        id: i64,
        status: ListStatus,
        progress: u32,
        snapshot: Option<(ListStatus, u32)>,
    ) {
        store.add_to_library(&sample(id), 100).unwrap();
        store.restore_list_status(id, status, progress, 100).unwrap();
        if let Some((s, p)) = snapshot {
            store.mark_synced(id, s, p).unwrap();
        }
    }

    fn state(store: &Store, id: i64) -> (ListStatus, u32, Option<ListStatus>, Option<u32>) {
        let s = store.get_show(id).unwrap().unwrap();
        (s.list_status, s.progress, s.synced_status, s.synced_progress)
    }

    #[test]
    fn reconcile_matrix_covers_every_cell() {
        // no/no: unchanged status, progress still maxes.
        let r = reconcile(Some((ListStatus::Watching, 5)), (ListStatus::Watching, 5), (ListStatus::Watching, 9));
        assert_eq!((r.status, r.progress, r.conflict), (ListStatus::Watching, 9, false));
        // progress-only remote bump rebaselines the snapshot too.
        assert_eq!((r.snapshot_status, r.snapshot_progress), (ListStatus::Watching, 9));

        // no/yes: adopt remote.
        let r = reconcile(Some((ListStatus::Planning, 0)), (ListStatus::Planning, 0), (ListStatus::Watching, 5));
        assert_eq!((r.status, r.progress, r.conflict), (ListStatus::Watching, 5, false));

        // yes/no: keep local; snapshot stays (base == remote).
        let r = reconcile(Some((ListStatus::Planning, 2)), (ListStatus::Watching, 4), (ListStatus::Planning, 2));
        assert_eq!((r.status, r.progress, r.conflict), (ListStatus::Watching, 4, false));
        assert_eq!((r.snapshot_status, r.snapshot_progress), (ListStatus::Planning, 2));

        // yes/yes same target: converged, no conflict.
        let r = reconcile(Some((ListStatus::Planning, 0)), (ListStatus::Completed, 12), (ListStatus::Completed, 10));
        assert_eq!((r.status, r.progress, r.conflict), (ListStatus::Completed, 12, false));

        // yes/yes different: keep local, conflict, snapshot is raw remote.
        let r = reconcile(Some((ListStatus::Planning, 0)), (ListStatus::Dropped, 3), (ListStatus::Watching, 8));
        assert_eq!((r.status, r.progress, r.conflict), (ListStatus::Dropped, 8, true));
        assert_eq!((r.snapshot_status, r.snapshot_progress), (ListStatus::Watching, 8));
    }

    #[test]
    fn reconcile_first_contact_treats_base_as_planning() {
        // base null: both sides "moved" from Planning; same target keeps local.
        let r = reconcile(None, (ListStatus::Watching, 4), (ListStatus::Watching, 2));
        assert_eq!((r.status, r.progress, r.conflict), (ListStatus::Watching, 4, false));
        // Snapshot re-baselines to the raw remote pair on first contact.
        assert_eq!((r.snapshot_status, r.snapshot_progress), (ListStatus::Watching, 2));
    }

    #[test]
    fn pull_adopts_remote_on_a_clean_row() {
        let store = Store::open_memory().unwrap();
        lib_row(&store, 1, ListStatus::Planning, 0, Some((ListStatus::Planning, 0)));
        let out = store.reconcile_pull(&[remote(1, ListStatus::Watching, 5)], 0).unwrap();
        assert_eq!(out, PullOutcome { reconciled: 1, ..Default::default() });
        assert_eq!(
            state(&store, 1),
            (ListStatus::Watching, 5, Some(ListStatus::Watching), Some(5))
        );
    }

    #[test]
    fn pull_conflict_keeps_local_and_stays_dirty() {
        let store = Store::open_memory().unwrap();
        lib_row(&store, 2, ListStatus::Dropped, 3, Some((ListStatus::Planning, 0)));
        let out = store.reconcile_pull(&[remote(2, ListStatus::Watching, 8)], 0).unwrap();
        assert_eq!(out.reconciled, 1);
        assert_eq!(out.conflicts, 1);
        // Local kept, progress maxed, snapshot = raw remote (server truth).
        assert_eq!(
            state(&store, 2),
            (ListStatus::Dropped, 8, Some(ListStatus::Watching), Some(8))
        );
        // synced_status (Watching) != list_status (Dropped) -> still on the push list.
        let dirty = store.list_dirty_for_sync().unwrap();
        assert!(dirty.iter().any(|r| r.anilist_id == 2));
    }

    #[test]
    fn pull_skips_a_fully_converged_row() {
        let store = Store::open_memory().unwrap();
        lib_row(&store, 3, ListStatus::Watching, 5, Some((ListStatus::Watching, 5)));
        let out = store.reconcile_pull(&[remote(3, ListStatus::Watching, 5)], 0).unwrap();
        assert_eq!(out, PullOutcome::default());
        assert_eq!(
            state(&store, 3),
            (ListStatus::Watching, 5, Some(ListStatus::Watching), Some(5))
        );
    }

    #[test]
    fn pull_reports_unmatched_remote_ids_without_importing() {
        let store = Store::open_memory().unwrap();
        lib_row(&store, 4, ListStatus::Planning, 0, None);
        let out = store
            .reconcile_pull(&[remote(4, ListStatus::Planning, 0), remote(999, ListStatus::Watching, 3)], 0)
            .unwrap();
        assert_eq!(out.unmatched, vec![999]);
        // The unmatched id was not minted into the library.
        assert!(store.get_show(999).unwrap().is_none());
    }

    #[test]
    fn pull_imports_a_watching_seed_into_the_library() {
        let store = Store::open_memory().unwrap();
        let out = store
            .reconcile_pull(&[remote_seed(700, ListStatus::Watching, 4, "Frieren")], 50)
            .unwrap();
        assert_eq!(out, PullOutcome { imported: 1, ..Default::default() });
        let show = store.get_show(700).unwrap().expect("row minted");
        assert_eq!(show.enrichment.title_romaji, "Frieren");
        assert_eq!(show.list_status, ListStatus::Watching);
        assert_eq!(show.progress, 4);
        assert_eq!(show.library_added_at, Some(50));
    }

    #[test]
    fn imported_row_is_born_clean_and_never_pushed_back() {
        let store = Store::open_memory().unwrap();
        store
            .reconcile_pull(&[remote_seed(705, ListStatus::Watching, 6, "Frieren")], 50)
            .unwrap();
        // synced_* stamped to match, so the import is not a local edit to sync.
        let dirty = store.list_dirty_for_sync().unwrap();
        assert!(!dirty.iter().any(|r| r.anilist_id == 705));
    }

    #[test]
    fn pull_does_not_import_non_watching_seeds() {
        let store = Store::open_memory().unwrap();
        let out = store
            .reconcile_pull(
                &[
                    remote_seed(701, ListStatus::Planning, 0, "Planned"),
                    remote_seed(702, ListStatus::Completed, 12, "Done"),
                ],
                50,
            )
            .unwrap();
        assert_eq!(out.imported, 0);
        assert_eq!(out.unmatched, vec![701, 702]);
        assert!(store.get_show(701).unwrap().is_none());
    }

    #[test]
    fn pull_leaves_a_titleless_watching_seed_count_only() {
        let store = Store::open_memory().unwrap();
        let out = store
            .reconcile_pull(&[remote_seed(703, ListStatus::Watching, 1, "")], 50)
            .unwrap();
        assert_eq!(out.imported, 0);
        assert_eq!(out.unmatched, vec![703]);
        assert!(store.get_show(703).unwrap().is_none());
    }

    #[test]
    fn pull_import_promotes_an_identity_row_without_wiping_enrichment() {
        let store = Store::open_memory().unwrap();
        // A richer identity row exists (bound provider, never library-added).
        let rich = Enrichment {
            anilist_id: 704,
            title_romaji: "Full Title".into(),
            cover_url: Some("https://img/cover.jpg".into()),
            ..Enrichment::default()
        };
        store.bind_provider(&rich, "senshi", "abc", 10).unwrap();
        assert!(store.get_show(704).unwrap().unwrap().library_added_at.is_none());

        // A sparse WATCHING seed for the same id promotes it into the library.
        let out = store
            .reconcile_pull(&[remote_seed(704, ListStatus::Watching, 2, "Seed Title")], 50)
            .unwrap();
        assert_eq!(out.imported, 1);
        let show = store.get_show(704).unwrap().unwrap();
        assert_eq!(show.library_added_at, Some(50));
        // Sparse seed did not clobber the richer existing cover.
        assert_eq!(show.enrichment.cover_url.as_deref(), Some("https://img/cover.jpg"));
    }

    #[test]
    fn import_leaves_a_concurrently_added_row_for_next_run() {
        // The import path's CAS guard, mirror of the matched-path race test: an
        // add-and-edit landing between the plan read and apply must survive, not
        // be clobbered by the import stamp.
        let path = tmp_db("import-cas.db");
        let store = Store::open(&path).unwrap();
        let (plan, imports, unmatched) = store
            .reconcile_plan(&[remote_seed(800, ListStatus::Watching, 5, "Frieren")])
            .unwrap();
        assert_eq!(imports.len(), 1, "the seed plans an import");

        // A second connection adds the show and sets a real status before apply.
        let other = Store::open(&path).unwrap();
        let e = Enrichment {
            anilist_id: 800,
            title_romaji: "Frieren".into(),
            ..Enrichment::default()
        };
        other.add_to_library(&e, 10).unwrap();
        other.restore_list_status(800, ListStatus::Completed, 20, 10).unwrap();

        let out = store.apply_reconcile(&plan, &imports, unmatched, 0).unwrap();
        assert_eq!(out.contended, 1);
        assert_eq!(out.imported, 0);
        // The concurrent edit survives; the mint rolled back.
        assert_eq!(state(&store, 800).0, ListStatus::Completed);
        assert_eq!(state(&store, 800).1, 20);
    }

    #[test]
    fn pull_rejects_a_control_only_title_seed() {
        // A lone control char strips to Some("") upstream; the guard must read
        // that as no title, not a present one (chaos pass).
        let store = Store::open_memory().unwrap();
        let entry = RemoteEntry {
            anilist_id: 706,
            status: ListStatus::Watching,
            progress: 1,
            import_seed: Some(Enrichment {
                anilist_id: 706,
                title_romaji: String::new(),
                title_english: Some(String::new()),
                ..Enrichment::default()
            }),
        };
        let out = store.reconcile_pull(&[entry], 50).unwrap();
        assert_eq!(out.imported, 0);
        assert_eq!(out.unmatched, vec![706]);
        assert!(store.get_show(706).unwrap().is_none());
    }

    #[test]
    fn pull_caps_import_count_and_counts_overflow_as_unmatched() {
        let store = Store::open_memory().unwrap();
        let entries: Vec<RemoteEntry> = (0..IMPORT_CAP as i64 + 10)
            .map(|i| remote_seed(1000 + i, ListStatus::Watching, 1, "Show"))
            .collect();
        let out = store.reconcile_pull(&entries, 50).unwrap();
        assert_eq!(out.imported as usize, IMPORT_CAP);
        assert_eq!(out.unmatched.len(), 10);
        // Kept slice is the lowest ids; overflow is the highest, count-only.
        assert!(out.unmatched.iter().all(|&id| id >= 1000 + IMPORT_CAP as i64));
    }

    #[test]
    fn pull_collapses_duplicate_remote_ids_keeping_max_progress() {
        let store = Store::open_memory().unwrap();
        lib_row(&store, 5, ListStatus::Watching, 0, Some((ListStatus::Watching, 0)));
        // Same id twice (custom-list duplication); the higher progress wins.
        let out = store
            .reconcile_pull(&[remote(5, ListStatus::Watching, 2), remote(5, ListStatus::Watching, 7)], 0)
            .unwrap();
        assert_eq!(out.reconciled, 1);
        assert_eq!(state(&store, 5).1, 7);
    }

    #[test]
    fn pull_leaves_a_concurrently_edited_row_for_next_run() {
        // The CAS guard: a local edit landing between the candidate read (plan)
        // and the write (apply) fails the guard, so the row is left untouched.
        let path = tmp_db("reconcile-cas.db");
        let store = Store::open(&path).unwrap();
        lib_row(&store, 6, ListStatus::Planning, 0, Some((ListStatus::Planning, 0)));

        let (plan, imports, unmatched) =
            store.reconcile_plan(&[remote(6, ListStatus::Watching, 5)]).unwrap();
        assert_eq!(plan.len(), 1, "the remote change should plan a write");

        // A concurrent edit lands through a second connection before apply.
        let other = Store::open(&path).unwrap();
        other.restore_list_status(6, ListStatus::Dropped, 9, 200).unwrap();

        let out = store.apply_reconcile(&plan, &imports, unmatched, 0).unwrap();
        assert_eq!(out.contended, 1);
        assert_eq!(out.reconciled, 0);
        // The concurrent edit survives; the stale merge did not overwrite it.
        assert_eq!(state(&store, 6).0, ListStatus::Dropped);
        assert_eq!(state(&store, 6).1, 9);
    }

    #[test]
    fn sync_dirty_set_tracks_the_live_pair() {
        let store = Store::open_memory().unwrap();
        // Identity rows never flood the push list.
        store
            .bind_provider(&sample(70), "senshi", "s70", 50)
            .unwrap();
        // Never-synced library row is dirty.
        store.add_to_library(&sample(71), 100).unwrap();
        let dirty = store.list_dirty_for_sync().unwrap();
        assert_eq!(dirty.len(), 1);
        assert_eq!(
            dirty[0],
            SyncRow {
                anilist_id: 71,
                title_romaji: "Show 71".into(),
                list_status: ListStatus::Planning,
                progress: 0,
            }
        );
        // Accepted pair goes clean.
        store.mark_synced(71, ListStatus::Planning, 0).unwrap();
        assert!(store.list_dirty_for_sync().unwrap().is_empty());
        // Any drift in the live pair re-dirties.
        store.record_play(71, 1, true, 200).unwrap();
        let dirty = store.list_dirty_for_sync().unwrap();
        assert_eq!(dirty.len(), 1);
        assert_eq!(dirty[0].progress, 1);
        assert_eq!(dirty[0].list_status, ListStatus::Watching);
    }

    #[test]
    fn blank_title_never_wipes_a_real_one() {
        let store = Store::open_memory().unwrap();
        let blank = Enrichment {
            anilist_id: 80,
            title_romaji: String::new(),
            ..Enrichment::default()
        };
        store.upsert_catalog_cache(&sample(80), 100, None).unwrap();
        store.upsert_catalog_cache(&blank, 200, None).unwrap();
        assert_eq!(
            store
                .get_catalog(80)
                .unwrap()
                .unwrap()
                .enrichment
                .title_romaji,
            "Show 80"
        );
        store.add_to_library(&sample(80), 100).unwrap();
        store.add_to_library(&blank, 200).unwrap();
        store.patch_show_enrichment(&blank, true, 300).unwrap();
        assert_eq!(
            store.get_show(80).unwrap().unwrap().enrichment.title_romaji,
            "Show 80"
        );
    }

    #[test]
    fn record_play_ignores_unknown_episode_index() {
        let store = Store::open_memory().unwrap();
        identity_row(&store, 81);
        store.record_play(81, 0, true, 100).unwrap();
        let show = store.get_show(81).unwrap().unwrap();
        assert_eq!(show.play_count, 0);
        assert_eq!(show.library_added_at, None, "index 0 must not join History");
    }

    #[test]
    fn save_progress_rejects_non_finite_floats() {
        let store = Store::open_memory().unwrap();
        identity_row(&store, 82);
        for (pos, dur) in [
            (f64::NAN, 100.0),
            (f64::INFINITY, 100.0),
            (f64::NEG_INFINITY, 100.0),
            (10.0, f64::NAN),
            (10.0, f64::INFINITY),
        ] {
            match store.save_progress(82, Translation::Sub, "1", pos, dur, None, 100) {
                Err(Error::NonFinitePosition { .. }) => {}
                other => panic!("expected NonFinitePosition for ({pos}, {dur}), got {other:?}"),
            }
        }
        assert_eq!(store.get_resume(82, Translation::Sub, "1").unwrap(), None);
    }

    #[test]
    fn newline_label_cannot_forge_cache_entries() {
        let store = Store::open_memory().unwrap();
        identity_row(&store, 83);
        let eps = vec!["1".to_string(), "2\nEVIL".to_string(), "3".to_string()];
        store
            .set_episode_cache(83, "senshi", Translation::Sub, &eps, Some("FINISHED"), 1000)
            .unwrap();
        assert_eq!(
            store
                .get_cached_episodes(83, "senshi", Translation::Sub, 1000)
                .unwrap(),
            Some(eps),
            "hostile label round-trips as one label, not two"
        );
    }

    /// Store is deliberately !Sync, so each racer opens its own handle on
    /// the same file: the real two-app-instances scenario.
    #[test]
    fn ratchet_survives_two_processes() {
        let path = tmp_db("ratchet-race.db");
        let main = Store::open(&path).unwrap();
        main.add_to_library(&sample(84), 50).unwrap();
        for round in 0..15 {
            main.restore_list_status(84, ListStatus::Watching, 0, 60)
                .unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            std::thread::scope(|s| {
                for ep in [20u32, 5] {
                    let barrier = std::sync::Arc::clone(&barrier);
                    let path = path.clone();
                    s.spawn(move || {
                        let store = Store::open(&path).unwrap();
                        barrier.wait();
                        store.record_play(84, ep, true, 100).unwrap();
                    });
                }
            });
            let progress = main.get_show(84).unwrap().unwrap().progress;
            assert_eq!(progress, 20, "ratchet regressed on round {round}");
        }
    }

    #[test]
    fn concurrent_rebind_never_errors_and_keeps_one_owner() {
        let path = tmp_db("bind-race.db");
        let main = Store::open(&path).unwrap();
        for round in 0..15 {
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            std::thread::scope(|s| {
                for id in [85, 86] {
                    let barrier = std::sync::Arc::clone(&barrier);
                    let path = path.clone();
                    s.spawn(move || {
                        let store = Store::open(&path).unwrap();
                        barrier.wait();
                        store
                            .bind_provider(&sample(id), "senshi", "dup", 100)
                            .unwrap();
                    });
                }
            });
            let owner = main.show_id_for_binding("senshi", "dup").unwrap();
            assert!(
                owner == Some(85) || owner == Some(86),
                "round {round}: edge lost, owner {owner:?}"
            );
            let edges: u32 = main
                .conn
                .query_row(
                    "SELECT count(*) FROM provider_binding \
                     WHERE provider = 'senshi' AND provider_id = 'dup'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(edges, 1, "round {round}: exactly one owner");
        }
    }

    #[test]
    fn meta_round_trip() {
        let store = Store::open_memory().unwrap();
        assert_eq!(store.meta_get("flag").unwrap(), None);
        store.meta_set("flag", "on").unwrap();
        assert_eq!(store.meta_get("flag").unwrap().as_deref(), Some("on"));
        store.meta_set("flag", "off").unwrap();
        assert_eq!(store.meta_get("flag").unwrap().as_deref(), Some("off"));
    }

    #[test]
    fn refresh_on_view_staleness_ladder() {
        let store = Store::open_memory().unwrap();
        let now = 1_000_000;

        assert!(store.enrichment_stale(7, now).unwrap(), "miss is stale");

        // A minted library row carries no stamp (the import-seed shape).
        store.add_to_library(&sample(7), now).unwrap();
        assert!(store.enrichment_stale(7, now).unwrap());

        // A stamped full answer is fresh until its status TTL lapses.
        assert!(store.patch_show_enrichment(&sample(7), true, now).unwrap());
        assert!(!store.enrichment_stale(7, now).unwrap());
        assert!(
            !store
                .enrichment_stale(7, now + ENRICH_TTL_FINISHED_SECS - 1)
                .unwrap()
        );
        assert!(
            store
                .enrichment_stale(7, now + ENRICH_TTL_FINISHED_SECS)
                .unwrap()
        );

        // Fieldset drift re-heals without waiting out the TTL (02 §5).
        store
            .conn
            .execute(
                "UPDATE show SET enrichment_fieldset_version = 0 WHERE anilist_id = 7",
                [],
            )
            .unwrap();
        assert!(store.enrichment_stale(7, now).unwrap());
    }

    #[test]
    fn staleness_falls_back_to_catalog_cache_expiry() {
        let store = Store::open_memory().unwrap();
        let now = 1_000_000;
        store
            .upsert_catalog_cache(&sample(9), now, Some(now + 100))
            .unwrap();
        assert!(!store.enrichment_stale(9, now).unwrap());
        assert!(store.enrichment_stale(9, now + 100).unwrap());
    }

    #[test]
    fn confirmed_null_stamp_is_update_only() {
        let store = Store::open_memory().unwrap();
        let now = 1_000_000;
        assert!(
            !store.stamp_enrichment_checked(9, now).unwrap(),
            "null never mints"
        );
        assert!(store.get_show(9).unwrap().is_none());

        store.add_to_library(&sample(9), now).unwrap();
        assert!(store.stamp_enrichment_checked(9, now).unwrap());
        assert!(!store.enrichment_stale(9, now).unwrap());
        assert_eq!(
            store.get_show(9).unwrap().unwrap().enrichment_fetched_at,
            Some(now)
        );
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
