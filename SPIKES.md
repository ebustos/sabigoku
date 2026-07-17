# sabigoku Spikes: the Rust mirror of zigoku's M0

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
| concurrency | hand-built `Channel(T)` on `Io.Mutex`/`Condition`, `io`-threaded locks, per-scope allocator choice → `mpsc::channel()` + `thread::spawn` + `for msg in rx` | almost none (home turf); the `drop(tx)` close idiom is a hang-footgun, and `move`/`Send` bounds force ownership thinking, but that thinking IS the race-freedom proof |
| stream | comptime `\"`-escape gymnastics, manual nonce/tag/ct array-slicing, 6-positional-arg `decrypt` the compiler can't check → typed `Nonce`/`Key` (swap = type error), tag-at-end IS the crate's `ct‖tag` convention | RustCrypto's trait + `GenericArray` maze (`new_from_slice`, `Aead`, implicit tag-append convention you must *know*); base64 0.22 API churn (no more `base64::decode`) |
| mpv | `io`-threaded `spawn`/`wait`, unmanaged-ArrayList argv, tagged-union `Term` (lowercase-tag gotcha) → `Command` + `.status()`; `?` collapses the multi-step resolve's error handling | spawn primitive is a dead heat; process spawning was already clean in Zig |

---

<!-- Per-spike write-ups land here as each one ships. -->

## 1. spike_http: HTTP + JSON

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

## 2. spike_sqlite: SQLite

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

## 3. spike_concurrency: threads + channel

zigoku earned this one the hard way. Its spike builds a generic `Channel(T)` by
hand, on `std.Io.Mutex` and `std.Io.Condition`, threading an `io` handle through
`lockUncancelable(io)` / `waitUncancelable(io, ...)` on every call, and choosing a
thread-safe `page_allocator` for cross-thread data versus a per-worker arena for
scratch. The spike's own comments warn that removing a `dupe` turns a returned
slice into a use-after-free the moment a worker's arena deinits.

The Rust version is `let (tx, rx) = mpsc::channel();`, `thread::spawn(move || …)`,
and `for msg in rx`. The channel is in std, generic, and typed. And the exact
use-after-free zigoku had to warn about in a comment is now a *compile error*:
the `move` closure transfers ownership into the worker, and `Send` bounds mean
the compiler refuses to let one thread read memory another owns.

**Deleted:** the entire hand-rolled channel, the mutex/condvar plumbing, the
`io` threading, and the per-scope allocator reasoning.

**Taxed:** this is Rust's home turf, so barely anything, but be fair about two
things. The `drop(tx)` that closes the channel is an invisible-footgun idiom:
forget it and `for msg in rx` blocks forever because a live sender still exists.
And the `move` / `'static` bounds on a spawned closure force you to think about
who owns what before it compiles. That second one isn't really a tax though, it's
the same safety zigoku bought with a hand-written comment and a "do this in a
scratch copy, it's instructive because it's wrong." Here the wrong version simply
doesn't build.

## 4. spike_stream: AES-256-GCM resolver

The reverse-engineering spike. zigoku decrypts the provider's `tobeparsed` blob:
`key = sha256(seed)`, base64 decode, then hand-slice `[0]` prefix, `[1..13]`
nonce, ciphertext, and the trailing 16-byte tag using the `raw[1..][0..12].*`
array-coercion idiom, and finally call `Aes256Gcm.decrypt` with six positional
arguments in an exact order. Its own comment admits the danger: "the type system
won't catch a swapped nonce/tag" because both are just byte arrays.

The Rust version keeps the same golden vectors (this spike asserts against the
exact blob and expected plaintext zigoku pins offline) but the crypto reads
differently. `Nonce` and `Key` are distinct types, so swapping them is a compile
error, not a silent `AuthenticationFailed`. And RustCrypto's AEAD convention is
"ciphertext with the tag appended," which is precisely how the blob already lays
out its bytes, so `&raw[13..]` goes straight in with no manual tag split. GCM
still fails closed exactly like Zig's: wrong key or offset returns `Err`.

**Deleted:** the comptime `\"`-escaping to build JSON-inside-JSON, the manual
nonce/tag/ciphertext slicing, and the unchecked argument ordering.

**Taxed:** RustCrypto is a trait-and-`GenericArray` maze. You have to import the
`Aead` trait to get `.decrypt`, reach for `new_from_slice`, and *know* the
implicit tag-append convention, which is nowhere in the call and bites silently
if you assume separate tag handling. base64 0.22 also churned its API hard:
`base64::decode` is gone, replaced by building a `GeneralPurpose` engine with an
explicit padding mode. Zig's `std.crypto` is one flat, explicit namespace you
read top to bottom; RustCrypto is safer once learned but assumes you already
speak its conventions.

## 5. spike_mpv: the pipeline capstone

The real end-to-end chain, one binary: search the live provider, fetch the
episode, decrypt the `tobeparsed` blob (the same crypto as spike_stream, now
against a live payload), resolve a playable URL (direct fast4speed, or follow a
`--<hex>` provider through clock.json), and spawn mpv on it. The headless probe
decodes one real frame off the CDN and exits 0. That exit code is the capstone's
whole point: the resolved stream actually plays. Verified live: `frieren` ->
"Sousou no Frieren" (28 sub eps) -> a fast4speed MP4 with an auth token -> frame
decoded.

The mpv spawn itself is the closest the two languages get. zigoku's is
`std.process.spawn(io, .{ .argv = … })` + `child.wait(io)`, building argv with an
unmanaged `ArrayList` and switching on a `Term` tagged union whose lowercase tags
cost a compile error; Rust's is a `Command` builder and `.status()`. Both inherit
stdio and pass user args straight through, so one binary serves a human and a CI
probe.

The interesting part isn't the spawn, it's *composing* the whole pipeline. Rust's
`?` and `let Some(x) = … else { continue }` thread errors and missing fields
through a five-step network+crypto flow without a single visible `match` ladder,
and the argv-injection guards (`consider` / `clean_arg`, printable-ASCII only,
reject a leading `--`) port over one-for-one because handing an untrusted CDN URL
to a subprocess is the same hazard in any language.

**Deleted:** the `io` threading and manual argv allocation (small), plus the
error-handling boilerplate the multi-step flow would otherwise need.

**Taxed:** the spawn primitive barely differs. Process spawning was already clean
in Zig, so on that one axis it's a near dead heat.

---

## Verdict (M0 complete)

Five spikes, five commits, and the pattern is clear enough to act on:

- **Rust wins big** exactly where zigoku spent the most hand-rolled effort:
  HTTP+JSON (serde deletes the writergate ceremony) and concurrency (std's
  channel plus a compile-time race proof replaces a hand-built `Channel(T)`).
  These are also the two areas that turned into daily *tax* in the real app,
  which is the whole reason this port exists.
- **Rust wins on safety** in the crypto: typed `Nonce`/`Key` make a whole class
  of argument-swap bug a compile error.
- **The wins have a bill:** a heavy dependency tree and real first-compile time,
  and an ecosystem (RustCrypto, base64 0.22, reqwest's hidden runtime) that
  assumes you speak its conventions. The verbosity didn't disappear; some of it
  moved from the call site into `Cargo.toml` and the type system.
- **Rust gives up** the thing zigoku's sqlite spike was built to celebrate:
  trivial C interop. Here a crate covered it. The day one doesn't exist, that's a
  cliff Zig doesn't have.
- **The mpv spawn is a wash**, and the capstone as a whole proves the real chain
  end to end: a live search resolves to a real stream that mpv actually plays.
  Where Rust helps at capstone scale is composing the five-step flow, not the
  spawn itself. Naming the wash is what keeps the rest of this ledger credible.

Net: for an app that is fundamentally HTTP-and-threads wearing a TUI, Rust
removes the exact friction that made zigoku a drag, and the tax it charges back
is mostly paid once (build graph, learning the ecosystem) rather than per-feature.
That's the case for the port, stated in receipts instead of vibes.

---

## 6. spike_cover: cover-art pipeline (post-M0, ROD-417)

```
cargo run --bin spike_cover                    # interactive grid, ROD-417
cargo run --bin spike_cover -- --halfblocks    # force the fallback path
cargo run --bin spike_cover -- --probe 10      # auto-quit, print a report
```

Not an M0 parity spike: zigoku got Kitty graphics natively from libvaxis, so
there is no Zig twin to ledger against. This one validates sabigoku's bet on
`ratatui-image` before M1 commits to hero cover art (DESIGN 3.3/3.8, support
matrix in 11.2): protocol detection, cell-pixel geometry and the adaptive cover
height derived from it, `Resize::Crop` into fixed cell blocks, halfblock
degrade, tmux survival, and worker-thread encode via `ThreadProtocol`.

**Measured (tmux, halfblocks path, 9 trending AniList covers):** detection
correctly lands on halfblocks with no reported cell size, so the DESIGN
fallback tiers (7/5-row cards, 28/20-row detail caps) engage; draw frames stay
around 1 ms worst-case; a full-grid re-encode after a terminal resize runs
~1.4 ms per cover off-thread; zero encode errors across resize storms into and
out of the narrow tier. No escape garbage inside tmux.

**Taxed:** `ThreadProtocol` counts request ids per instance, so N images
sharing one worker channel cannot route responses by trial (colliding ids would
install the wrong poster). The spike gives each card a private request channel
drained after every draw into an index-tagged worker queue; the real app needs
the same shape. And the dependency avalanche returns: `image` plus
ratatui-image's wezterm helper crates roughly double `Cargo.lock`.

**Kitty path ratified in ghostty (Rod, 2026-07-17):** protocol detected, cell
pixels reported (9x20), adaptive `cover_h=13` matching the 2:3 derivation
exactly, crop-to-fill clean, ~4.4 ms mean off-thread encode, zero errors.
kitty and wezterm are spot-checks when convenient; ratatui-image drives all
three through the same protocol branch.
