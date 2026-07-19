//! spike_sqlite: schema, a user_version migration, and an upsert.
//! Parity: zigoku spike #2 (sqlite_store.zig, ROD-56).
//!
//! In zigoku this spike existed to prove Zig's C-interop superpower: `@cImport`
//! the sqlite3 header and drive the C API directly. Rust makes the opposite
//! point: you don't touch C at all. `rusqlite` with the `bundled` feature
//! compiles sqlite from source into the binary and hands you a safe API.
//!
//! Run: cargo run --bin spike_sqlite

use rusqlite::{Connection, params};

const DB_PATH: &str = "/tmp/sabigoku-spike.db";
const SCHEMA_VERSION: i64 = 1;

// PRAGMA user_version is a free integer in the DB header. Read it, apply forward
// steps, stamp the new version. The honest migration pattern, same as zigoku.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if v < 1 {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS anime (
                 anilist_id INTEGER PRIMARY KEY,
                 title      TEXT NOT NULL,
                 episodes   INTEGER,
                 status     TEXT NOT NULL DEFAULT 'planning'
             );
             CREATE TABLE IF NOT EXISTS episode_progress (
                 anilist_id    INTEGER NOT NULL REFERENCES anime(anilist_id) ON DELETE CASCADE,
                 episode       INTEGER NOT NULL,
                 position_secs INTEGER NOT NULL,
                 PRIMARY KEY (anilist_id, episode)
             );
             PRAGMA user_version = 1;",
        )?;
    }
    Ok(())
}

fn main() -> rusqlite::Result<()> {
    let conn = Connection::open(DB_PATH)?;
    // FK enforcement is per-connection and off by default in sqlite.
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;

    let before: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    migrate(&conn)?;
    let after: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    println!("migration: user_version {before} -> {after} (target {SCHEMA_VERSION})");

    // Upsert the catalog rows. Re-running the spike is a no-op, not a duplicate.
    let upsert_anime = "INSERT INTO anime (anilist_id, title, episodes, status)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(anilist_id) DO UPDATE SET
             title = excluded.title, episodes = excluded.episodes";
    conn.execute(
        upsert_anime,
        params![154587, "Frieren: Beyond Journey's End", 28, "watching"],
    )?;
    conn.execute(upsert_anime, params![9253, "Steins;Gate", 24, "planning"])?;

    // Prove the ON CONFLICT upsert on progress: write episode 1 twice, last wins.
    let upsert_progress = "INSERT INTO episode_progress (anilist_id, episode, position_secs)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(anilist_id, episode) DO UPDATE SET position_secs = excluded.position_secs";
    conn.execute(upsert_progress, params![154587, 1, 120])?;
    conn.execute(upsert_progress, params![154587, 1, 540])?; // 120s -> 540s, last write wins

    // Read back with a typed row mapper. `query_map` yields Result<T> per row.
    println!("\nwatchlist:");
    let mut stmt = conn.prepare(
        "SELECT a.anilist_id, a.title, a.status, a.episodes,
                COALESCE(p.position_secs, 0)
         FROM anime a
         LEFT JOIN episode_progress p
             ON p.anilist_id = a.anilist_id AND p.episode = 1
         ORDER BY a.title",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (id, title, status, episodes, pos) = row?;
        let eps = episodes.map_or_else(|| "?".into(), |n| n.to_string());
        println!("  [{id:>6}] {title}  ({eps} eps, {status})  ep1 @ {pos}s");
    }
    Ok(())
}
