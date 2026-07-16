//! sabigoku: a Rust port of zigoku, one spike at a time.
//!
//! The real entry point is each spike binary under `src/bin/`. This stub just
//! lists them so `cargo run` isn't a dead end.

fn main() {
    print!(
        "\
sabigoku spikes (M0 parity with zigoku/SPIKES.md)

  cargo run --bin spike_http        -- frieren     # HTTP + JSON (AniList)
  cargo run --bin spike_sqlite                      # SQLite + migration + upsert
  cargo run --bin spike_concurrency -- frieren      # threads + channel
  cargo run --bin spike_stream      -- frieren      # AES-256-GCM resolver
  cargo run --bin spike_mpv         -- frieren      # full pipeline -> mpv

Each spike is a single idea with no framework around it. Read them in order.
"
    );
}
