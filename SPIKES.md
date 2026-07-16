# sabigoku Spikes — the Rust mirror of zigoku's M0

Five throwaway programs that prove sabigoku's riskiest unknowns *in isolation*.
Each mirrors a zigoku spike one-for-one, same job, same provider, so the only
variable is the language. This file is the DX journal: for every spike, what the
Rust ecosystem **deleted** versus what Rust **taxed** compared to the Zig 0.16
original in `../zigoku/SPIKES.md`.

The rule for keeping the comparison honest: building the second version of an app
you already understand is always faster, and that speed is *not* the language.
So the ledger below only counts things attributable to the language and its
ecosystem, not to me already knowing the problem.

```
cargo run --bin spike_http        -- frieren     # ROD-403  HTTP + JSON
cargo run --bin spike_sqlite                      # ROD-404  SQLite + migration
cargo run --bin spike_concurrency -- frieren      # ROD-405  threads + channel
cargo run --bin spike_stream      -- frieren      # ROD-406  AES-GCM resolver
cargo run --bin spike_mpv         -- frieren      # ROD-407  full pipeline -> mpv
```

---

## The ledger (running tally)

| Spike | Ecosystem deleted | Rust taxed |
|---|---|---|
| http | _tbd_ | _tbd_ |
| sqlite | _tbd_ | _tbd_ |
| concurrency | _tbd_ | _tbd_ |
| stream | _tbd_ | _tbd_ |
| mpv | _tbd_ | _tbd_ |

---

<!-- Per-spike write-ups land here as each one ships. -->
