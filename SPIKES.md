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
| http | writer/flush dance, manual `std.http.Client`+`io`, hand-matched structs → `derive` + `.json()` | full async runtime (tokio) + rustls pulled in for a *blocking* call; ~100 crates, real first-compile cost |
| sqlite | _tbd_ | _tbd_ |
| concurrency | _tbd_ | _tbd_ |
| stream | _tbd_ | _tbd_ |
| mpv | _tbd_ | _tbd_ |

---

<!-- Per-spike write-ups land here as each one ships. -->

## 1. spike_http — HTTP + JSON

The Zig original is the clearest "verbose but explicit" showcase in the whole
project: you provide an output buffer, build an `std.http.Client` with an `io`
handle, stream the response into a `Writer.Allocating`, pull `.buffered()`, then
declare structs that `std.json.parseFromSlice` matches by name. Every allocation
is visible; every byte of I/O is yours to flush.

The Rust version does the same job in three moves: `#[derive(Serialize)]` on the
request, `#[derive(Deserialize)]` on the response, and `.json()` on both ends of
`reqwest`. No buffer, no flush, no allocator threading, no CA-bundle branch
(rustls ships its own roots).

**Deleted:** the writergate ceremony and manual JSON wiring, maybe 40 lines of
Zig collapsed to ~10 of declarations.

**Taxed:** dependencies. `reqwest::blocking` is a thin wrapper that still drags a
full async runtime and a TLS stack underneath, so a synchronous one-shot call
pulls ~100 transitive crates and a real first-compile wait. Zig's spike had zero
dependencies beyond std. And serde over borrowed data means lifetime parameters
(`Request<'a>`) the Zig version never had to spell out. The verbosity didn't
vanish; it moved from the call site into the build graph.
