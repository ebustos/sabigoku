# sabigoku · 錆獄

[![CI](https://github.com/vantroy/sabigoku/actions/workflows/ci.yml/badge.svg)](https://github.com/vantroy/sabigoku/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/vantroy/sabigoku?color=B7410E)](https://github.com/vantroy/sabigoku/releases/latest)
[![crates.io](https://img.shields.io/crates/v/sabigoku)](https://crates.io/crates/sabigoku)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPL--3.0--or--later-blue)](https://github.com/vantroy/sabigoku/blob/master/LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.90%2B-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS-555)](https://github.com/vantroy/sabigoku/releases/latest)

A terminal anime browser and player: search a catalogue, keep a watchlist, and
play episodes in `mpv`, with real cover art and AniList sync.

> *Sabi + jigoku ("rust hell").* A from-scratch Rust port of
> [zigoku](https://github.com/vantroy/zigoku), built as a controlled comparison:
> rebuild the same app's riskiest pieces in Rust and measure what the language
> and its ecosystem actually change.
> [`SPIKES.md`](https://github.com/vantroy/sabigoku/blob/master/SPIKES.md) is
> that ledger. Released and in active development.

## Contents

- [Screenshots / Demo](#screenshots--demo)
- [What it does today](#what-it-does-today)
- [Install](#install)
- [Staying up to date](#staying-up-to-date)
- [Development](#development)
- [Stack](#stack)
- [Why this exists](#why-this-exists)
- [Acknowledgements](#acknowledgements)
- [License](#license)

## Screenshots / Demo

![Demo: live watchlist cover art, filtered down to a title's detail](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/demo.gif)

*Hero: real cover art, painted straight to the framebuffer via the Kitty
graphics protocol, no halfblocks or ASCII. The watchlist's cover repaints on
every selection, a filter narrows the list to one title, and its detail rests
on cover, kanji chips, and synopsis.*

---

![Discover: a ranked wall of real cover art, sweeping and reloading live](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/discover.gif)

*Discover: a ranked wall of real cover art, ten shows a screen, across
`Trending` / `Popular` / `Top Rated` / `This Season`. Moving the selection
sweeps cover to cover; switching the ranking axis reloads a fresh wall, live
from the AniList rankings.*

![Discover detail: cover, kanji chips, score, synopsis, and episode grid](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/detail-cover.png)

*Discover detail: one result opened, with real cover art, kanji metadata
chips, AniList score, synopsis, and the episode grid in a single pane.*

---

![Browse: live catalogue search, results and cover art streaming in](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/browse.gif)

*Browse: type a query and the catalogue search runs live. Results and their
cover art stream into the two-pane view, and opening a result lands on its
detail.*

![Watchlist: grouped status headers, progress bars, cover on selection](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/watchlist.png)

*Watchlist: grouped status headers, per-show progress bars, and the real cover
and metadata for whichever title is selected.*

![Themes tour: Settings palette cycle re-theming a live detail view](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/stills.gif)

*Themes tour: cycle the palette in Settings, `terminal_ghost` → `phosphor` →
`nord` → `tokyonight`, then jump back to a live detail to see the re-theme
already applied app-wide.*

---

## What it does today

- **Full TUI** (ratatui): two-pane shell with live catalogue search, a detail
  pane, grouped watchlist, ranked Discover feed, toasts, and a status bar that
  always shows the keys that work right now.
- **Watchlist & watch-state**: planning / watching / paused / dropped /
  completed, with live filtering; the list refreshes automatically after
  playback.
- **Discover**: an AniList-backed ranked feed across `Trending` / `Popular` /
  `Top Rated` / `This Season`, with real cover art and score badges.
- **Cover art**: real pixels in terminals that support the graphics protocol
  (kitty, ghostty, WezTerm, iTerm2), halfblock cells elsewhere.
- **Search → resolve → play**: catalogue search and metadata come from
  AniList; a result resolves to a streaming source and plays in `mpv`.
- **Multiple streaming sources, with automatic fallback**: if one source
  fails, the next is tried automatically, with a source-order preference and
  per-show pins.
- **History & exact resume** (SQLite): watch history and resume positions,
  checkpointed during playback.
- **AniList account sync**: connect from Settings or `sabigoku login`; the
  watchlist syncs both ways in the background, or stays local by choice.
- **Config & settings**: a live-editable Settings tab, persisted to
  `~/.config/sabigoku/config.toml`. Four palettes: `terminal_ghost`,
  `phosphor`, `nord`, `tokyonight`.
- **Self-updating**: a boot check toasts when a newer release exists;
  `sabigoku update` upgrades in place.
- **Scriptable CLI**: `sabigoku <query>` runs a search → pick → play flow,
  headless-friendly.

## Install

One hard runtime dependency across every install method: `mpv`. The binary
shells out to whatever `mpv` is on your `PATH` to play video; without it, you
get a browser.

### Quick install (Linux & macOS)

```sh
curl -fsSL https://raw.githubusercontent.com/vantroy/sabigoku/master/install.sh | sh
```

Detects your OS and architecture, verifies the download against the published
`sha256sums.txt`, and installs `sabigoku` to `~/.local/bin`. Covers Linux
x86_64/aarch64 and macOS Apple Silicon; Intel Macs get pointed at
`cargo install`.

```sh
# pin a release instead of the latest:
curl -fsSL https://raw.githubusercontent.com/vantroy/sabigoku/master/install.sh | SABIGOKU_VERSION=0.1.1 sh

# install somewhere other than ~/.local/bin:
curl -fsSL https://raw.githubusercontent.com/vantroy/sabigoku/master/install.sh | BINDIR=/usr/local/bin sh
```

### AUR (Arch Linux)

```sh
paru -S sabigoku       # or: yay -S sabigoku
```

Source build: compiles the tagged release with cargo, shells out to `mpv` at
runtime. Recipe:
[`PKGBUILD`](https://github.com/vantroy/sabigoku/blob/master/packaging/aur/PKGBUILD).

```sh
git clone https://aur.archlinux.org/sabigoku.git
cd sabigoku && makepkg -si
```

### macOS: Homebrew

```sh
brew install vantroy/sabigoku/sabigoku
```

Taps [`vantroy/homebrew-sabigoku`](https://github.com/vantroy/homebrew-sabigoku)
automatically. Installs the prebuilt Apple Silicon binary and pulls `mpv` as a
dependency; upgrades via `brew upgrade`. Intel Macs: `cargo install` instead.

### crates.io

```sh
cargo install sabigoku
```

Builds from source on any platform with Rust 1.90+. SQLite and TLS (rustls)
are compiled in; nothing to link against.

### Prebuilt binary

Linux tarballs are static musl builds (no glibc); macOS ships an Apple
Silicon binary.

1. Download the tarball for your machine from the
   [latest release](https://github.com/vantroy/sabigoku/releases/latest):

   | Machine | File |
   |---|---|
   | x86_64 Linux | `sabigoku-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz` |
   | aarch64 Linux (ARM64) | `sabigoku-vX.Y.Z-aarch64-unknown-linux-musl.tar.gz` |
   | macOS Apple Silicon | `sabigoku-vX.Y.Z-aarch64-apple-darwin.tar.gz` |

2. Verify against `sha256sums.txt` from the same release page:

   ```sh
   sha256sum -c --ignore-missing sha256sums.txt
   ```

3. Extract and put it on your `PATH`:

   ```sh
   tar -xzf sabigoku-vX.Y.Z-<target>.tar.gz
   mv sabigoku ~/.local/bin/       # or wherever your PATH points
   ```

### From source

```sh
git clone https://github.com/vantroy/sabigoku.git
cd sabigoku
cargo build --release            # -> target/release/sabigoku
```

Requires Rust 1.90+ (edition 2024). No system SQLite or OpenSSL needed.

---

Once installed:

```sh
sabigoku                     # no args → the TUI
sabigoku frieren             # CLI flow: search → pick → play
sabigoku "cowboy bebop" --dub
sabigoku login               # connect an AniList account
sabigoku sync                # one-shot watchlist sync
sabigoku update              # self-update to the latest release
sabigoku --paths             # where config, data and cache live
sabigoku --debug             # diagnostics: stderr (CLI) or the log file (TUI)
```

## Staying up to date

On boot, the TUI checks for a newer release (cached, at most once an hour)
and shows a toast when one exists; the Settings tab has the off-switch.
`sabigoku update` does a fresh check: an installer-managed binary is replaced
in place, and a package-managed one (AUR, Homebrew, cargo) gets the right
upgrade command printed instead of a side-channel overwrite.

## Development

To work on sabigoku without installing:

```sh
cargo run                     # no args → the TUI
cargo run -- frieren          # CLI flow: search → pick → play
cargo test                    # offline-safe; live-network suites are opt-in (--ignored)
```

The spikes in
[`examples/`](https://github.com/vantroy/sabigoku/tree/master/examples)
de-risked the hard unknowns before the real architecture existed. Each runs
on its own:

```sh
cargo run --example spike_http          # AniList HTTP search
cargo run --example spike_sqlite       # SQLite via rusqlite
cargo run --example spike_concurrency  # thread pool + channel
cargo run --example spike_stream       # stream resolver
cargo run --example spike_mpv          # full pipeline → play in mpv
cargo run --example spike_cover        # Kitty-graphics cover smoke test
```

**[SPIKES.md](https://github.com/vantroy/sabigoku/blob/master/SPIKES.md)** is
the annotated tour, written spike by spike against the Zig originals.

The port itself is documented as product law:
[`PORT.md`](https://github.com/vantroy/sabigoku/blob/master/PORT.md) and
[`docs/port/`](https://github.com/vantroy/sabigoku/tree/master/docs/port) map
what zigoku does and how each piece lands in Rust, and
[`DESIGN.md`](https://github.com/vantroy/sabigoku/blob/master/DESIGN.md) holds
the design system, layout grammar, and UI contracts.

## Stack

- **TUI:** ratatui + crossterm, with ratatui-image for Kitty graphics and the
  halfblock fallback
- **Storage:** SQLite via rusqlite, compiled in (`bundled`)
- **HTTP:** reqwest (blocking) over rustls; no system TLS or OpenSSL
- **Concurrency:** worker threads + channels; no async runtime
- **Streaming:** multiple sources behind one provider interface, with automatic
  fallback between them
- **Catalog:** AniList, backing search, Discover, metadata, cover art, and
  account sync

## Why this exists

[zigoku](https://github.com/vantroy/zigoku) was built to learn Zig on a real
app: networking, C interop, threads, a TUI, and a database. sabigoku asks the
follow-up question: rebuild the same app in Rust, feature for feature, and see
what the language and its ecosystem actually change. Same views, same store
semantics, same behavior contracts; different memory model, different
libraries, different failure modes. The comparison starts in
[`SPIKES.md`](https://github.com/vantroy/sabigoku/blob/master/SPIKES.md), where
each risky piece got a spike against its Zig original before any real module
existed, and continues through the port docs, which record every place the
Rust version deliberately diverges.

The same disclosure zigoku makes applies here: most of the code is written by
AI, a personal agent setup driving an ensemble of models organized as a small
crew for implementation, review, and verification, while the human side owns
the architecture, the milestone planning, the design decisions, and the review
of everything that lands. The learning happens a layer up: studying the
generated code, questioning its choices, and understanding every line well
enough to direct the next one.

## Acknowledgements

- **[zigoku](https://github.com/vantroy/zigoku)**, the reference. sabigoku is
  a port, and the port docs cite it chapter and verse; its repo tells the
  original story, including the acknowledgements this project inherits.
- **[anipy-cli](https://github.com/sdaqo/anipy-cli)** by
  [sdaqo](https://github.com/sdaqo) (GPL-3.0): zigoku's original streaming
  support followed a trail first opened there, reimplemented rather than
  copied, and the GPL lineage noted in [License](#license) carries through
  this port. Thank you. 🙏
- Catalogue metadata & cover art from **[AniList](https://anilist.co/)**.

## License

[GPL-3.0-or-later](https://github.com/vantroy/sabigoku/blob/master/LICENSE),
the same as zigoku. This is a port of a GPL project, and the lineage runs back
one step further: zigoku's original streaming support followed a trail first
opened by [anipy-cli](https://github.com/sdaqo/anipy-cli) (GPL-3.0),
reimplemented rather than copied. The license keeps that lineage unambiguous.
