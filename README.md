# sabigoku · 錆獄

A terminal anime browser and player: search a catalogue, keep a watchlist, and
play episodes in `mpv`, with cover art and AniList sync.

錆獄 ("rust hell") is a from-scratch Rust port of
[zigoku](https://github.com/vantroy/zigoku), built as a controlled comparison:
rebuild the same app's riskiest pieces in Rust and measure what the language and
its ecosystem actually change.
[`SPIKES.md`](https://github.com/vantroy/sabigoku/blob/master/SPIKES.md) is that
ledger.

## Status

Released and in active development. Working today: browse and search, detail
view, a Discover feed, the watchlist, playback with exact resume, AniList login
and two-way sync, and settings with four palettes.

## Install

Needs `mpv` on `PATH` to play anything.

```sh
curl -fsSL https://raw.githubusercontent.com/vantroy/sabigoku/master/install.sh | sh
```

Downloads the build for your machine, checks it against the published
`sha256sums.txt`, and installs to `~/.local/bin` (`BINDIR` to change that,
`SABIGOKU_VERSION` to pin a release). Linux x86_64 and aarch64, macOS arm64.

With a Rust toolchain, which is also the route for Intel Macs:

```sh
cargo install sabigoku
```

Tarballs for each target are attached to every
[release](https://github.com/vantroy/sabigoku/releases). AUR and Homebrew are on
the way.

From source, with Rust (edition 2024):

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

- [`PORT.md`](https://github.com/vantroy/sabigoku/blob/master/PORT.md) and [`docs/port/`](https://github.com/vantroy/sabigoku/tree/master/docs/port): what zigoku does, and how each piece maps to Rust
- [`DESIGN.md`](https://github.com/vantroy/sabigoku/blob/master/DESIGN.md): the design system, layout grammar and UI contracts
- [`SPIKES.md`](https://github.com/vantroy/sabigoku/blob/master/SPIKES.md): the Rust-versus-Zig ledger from the M0 spikes

## License

[GPL-3.0-or-later](https://github.com/vantroy/sabigoku/blob/master/LICENSE), the
same as zigoku. This is a port of a GPL project, and the lineage runs back one
step further: zigoku's original
streaming support followed a trail first opened by
[anipy-cli](https://github.com/sdaqo/anipy-cli) (GPL-3.0), reimplemented rather
than copied. The license keeps that lineage unambiguous.

Catalogue metadata and cover art come from [AniList](https://anilist.co/).
