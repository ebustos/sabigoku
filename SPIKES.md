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
| sqlite | `@cImport` + the whole C wrapper layer (bind/col/null-ptr handling) → `params![]` + `query_map`; `bundled` kills the system-link and the macOS unbundled-sqlite segfault class | `bundled` compiles sqlite C on first build; and you *gave up* the interop the Zig spike existed to prove |
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

## 2. spike_sqlite — SQLite

This is the spike where the two languages argue about their whole reason to
exist. Zig's version is a *showcase*: `@cImport("sqlite3.h")` and you drive the C
API directly, wrapping its sharp edges (`?*c.sqlite3` null pointers, the
`SQLITE_STATIC` destructor promise, `sqlite3_column_bytes` length reads) in a
tidy helper layer. The trivial C interop *is the point*, the thing you'd reach
for constantly in a Zig codebase.

Rust makes the opposite argument: you never see C. `rusqlite` gives you
`params![]` binding and `query_map` with a typed `Result<T>` row mapper, and the
`bundled` feature compiles sqlite from source straight into the binary. That last
part deletes an entire bug class we hit in zigoku on macOS, where an *unbundled*
system sqlite segfaulted at runtime. Here there is no system library to link,
version-match, or ship.

**Deleted:** the C boundary and its whole hand-written wrapper, plus the
system-library packaging risk.

**Taxed:** two things, and the second is the interesting one. First, `bundled`
runs a C compile on the first build, so cold builds are slower. Second, and more
honest: this spike *surrendered the exact capability the Zig one was proving*.
Rust can do C FFI, but it walls it behind `unsafe` and defers to "someone already
wrote the crate." That's pure win when the crate exists, like here. It's a cliff
the day you need to bind a C library nobody has wrapped yet, which in Zig is a
Tuesday.
