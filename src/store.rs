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

use rusqlite::{Connection, TransactionBehavior};

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

#[derive(Debug)]
pub struct Store {
    // dead_code: no reader until the first query method lands (chunk 2);
    // deliberately NOT pub, raw SQL must stay unreachable outside this module.
    #[allow(dead_code)]
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
        perms.set_readonly(false);
        std::fs::set_permissions(&path, perms).unwrap();
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
