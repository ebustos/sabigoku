# sabigoku · 錆獄

A terminal anime browser and player: search a catalogue, keep a watchlist, and
play episodes in `mpv`, with cover art and AniList sync.

錆獄 ("rust hell") is a from-scratch Rust port of
[zigoku](https://github.com/vantroy/zigoku), built as a controlled comparison:
rebuild the same app's riskiest pieces in Rust and measure what the language and
its ecosystem actually change. [`SPIKES.md`](SPIKES.md) is that ledger.

## Status

In development. No tagged release yet, so the only way in is a source build.
Working today: browse and search, detail view, a Discover feed, the watchlist,
playback with exact resume, AniList login and two-way sync, and settings with
four palettes.

Packaged installs (prebuilt binaries, `curl | sh`, AUR, Homebrew) arrive with
the first release, and this file grows an Install section when they do.

## Build

Needs Rust (edition 2024), and `mpv` on `PATH` to play anything.

```sh
git clone https://github.com/vantroy/sabigoku.git
cd sabigoku
cargo build --release        # -> target/release/sabigoku
```

## Use

```sh
sabigoku                     # the TUI
sabigoku frieren             # search, pick, play
sabigoku "cowboy bebop" --dub
sabigoku login               # connect an AniList account
sabigoku sync                # sync the watchlist
sabigoku --paths             # where config, data and cache live
```

Cover art renders as real images in terminals that answer the graphics
capability query (kitty, ghostty, WezTerm, iTerm2) and as halfblock cells
everywhere else.

## Docs

- [`PORT.md`](PORT.md) and [`docs/port/`](docs/port/): what zigoku does, and how each piece maps to Rust
- [`DESIGN.md`](DESIGN.md): the design system, layout grammar and UI contracts
- [`SPIKES.md`](SPIKES.md): the Rust-versus-Zig ledger from the M0 spikes
