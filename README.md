# sabigoku

A from-scratch Rust port of [zigoku](https://github.com/vantroy/zigoku), a
terminal anime browser and player. Same app, different language, built on purpose
as a controlled developer-experience comparison: rebuild each of zigoku's riskiest
pieces in Rust and measure what the language and its ecosystem actually change.

錆獄, "rust hell," to zigoku's 地獄. The joke writes itself.

## Why this exists

zigoku is written in Zig 0.16, and for an app that is fundamentally HTTP and
threads wearing a TUI, the parts Zig makes you hand-roll (an HTTP retry spine, a
channel, timing primitives) turned into recurring friction. This port is the
experiment that answers the open question directly: does Rust's ecosystem remove
that friction, and what does it charge back?

The rule that keeps the comparison honest: building the second version of an app
you already understand is always faster, and that speed is *not* the language. So
the analysis only counts what is attributable to the language and its ecosystem,
never to already knowing the problem.

## Status

**M0 complete.** Five throwaway spikes prove the riskiest unknowns in isolation,
each mirroring a zigoku spike one-for-one. This is a spike repo, not the app yet;
M1 is where the ideas collapse into real modules behind interfaces.

## The spikes

```
cargo run --bin spike_http        -- frieren     # HTTP + JSON (AniList)
cargo run --bin spike_sqlite                      # SQLite + migration + upsert
cargo run --bin spike_concurrency -- frieren      # threads + channel
cargo run --bin spike_stream                      # AES-256-GCM resolver crypto
cargo run --bin spike_mpv         -- frieren --frames=1 --vo=null --no-audio
```

| Spike | Proves | Rust stack |
|---|---|---|
| `spike_http` | HTTPS POST + typed JSON against AniList | reqwest, serde |
| `spike_sqlite` | schema, `user_version` migration, upsert | rusqlite (bundled) |
| `spike_concurrency` | N workers fetch, results over a channel | std::thread, mpsc |
| `spike_stream` | decrypt the provider's stream blob | aes-gcm, sha2, base64 |
| `spike_mpv` | search to resolve to a playing stream | reqwest + the above + std::process |

Four spikes are deterministic and offline (the crypto runs pinned golden
vectors). `spike_mpv` talks to a live provider end to end, so it is a smoke test,
not a unit test: if the upstream hashes rotate or the CDN refuses, it fails loudly
with a clear message rather than pretending.

## The comparison

The full ledger lives in [`SPIKES.md`](SPIKES.md): for every spike, what the Rust
ecosystem **deleted** versus what Rust **taxed**, measured against the Zig 0.16
original. The short version:

- **Rust wins biggest** where zigoku hand-rolled the most: HTTP+JSON (serde
  deletes the manual writer/flush and JSON wiring) and concurrency (std's channel
  plus a compile-time race proof replaces a hand-built generic channel).
- **Rust wins on safety** in the crypto: typed `Nonce`/`Key` make an
  argument-swap bug a compile error instead of a silent auth failure.
- **The wins have a bill:** a heavy dependency tree, real first-compile time, and
  an ecosystem that assumes you speak its conventions. The verbosity did not
  vanish; some of it moved into `Cargo.toml` and the type system.
- **Rust gives up** the thing zigoku's SQLite spike was built to celebrate:
  trivial C interop. Here a crate covered it. The day one does not exist is a
  cliff Zig does not have.
- **Process spawning is a wash.** Not everything is a blowout, and saying so is
  what keeps the rest of the ledger credible.

## Requirements

- Rust (edition 2024)
- `mpv` on `PATH` for `spike_mpv`

## Layout

Each spike is a standalone binary under `src/bin/`, one isolated idea with no
framework around it. Cargo auto-discovers them as targets, no manifest wiring.
Read them in the order above; they double as the shortest tour of the stack.
