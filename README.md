# sabigoku · 錆獄

[![CI](https://github.com/vantroy/sabigoku/actions/workflows/ci.yml/badge.svg)](https://github.com/vantroy/sabigoku/actions/workflows/ci.yml)
[![Latest release](https://img.shields.io/github/v/release/vantroy/sabigoku?color=B7410E)](https://github.com/vantroy/sabigoku/releases/latest)
[![crates.io](https://img.shields.io/crates/v/sabigoku)](https://crates.io/crates/sabigoku)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/License-GPL--3.0--or--later-blue)](https://github.com/vantroy/sabigoku/blob/master/LICENSE)
[![Rust](https://img.shields.io/badge/Rust-1.90%2B-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Platform](https://img.shields.io/badge/platform-Linux%20%7C%20macOS-555)](https://github.com/vantroy/sabigoku/releases/latest)

Terminal anime browser and player. Search the catalogue, keep a watchlist, play
episodes in `mpv`. Real cover art. AniList sync.

> *Sabi + jigoku ("rust hell").* From-scratch Rust port of
> [zigoku](https://github.com/vantroy/zigoku): same app, different language, so
> the comparison is real.
> [`SPIKES.md`](https://github.com/vantroy/sabigoku/blob/master/SPIKES.md) is
> the ledger. Released and in active development.

## Contents

- [Screenshots](#screenshots)
- [Features](#features)
- [Install](#install)
- [Staying up to date](#staying-up-to-date)
- [Development](#development)
- [Stack](#stack)
- [Why this exists](#why-this-exists)
- [Acknowledgements](#acknowledgements)
- [License](#license)

## Screenshots

![Demo: watchlist cover art and title detail](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/demo.gif)

Watchlist cover art over the Kitty graphics protocol. Filter down, open detail.

---

![Discover: ranked cover wall](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/discover.gif)

Discover: `Trending` / `Popular` / `Top Rated` / `This Season`, live from AniList.

![Discover detail: cover, metadata, synopsis, episodes](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/detail-cover.png)

Detail: cover, kanji chips, score, synopsis, episode grid.

---

![Browse: live catalogue search](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/browse.gif)

Browse: type a query, results and covers stream in.

![Watchlist: status groups and progress](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/watchlist.png)

Watchlist: status headers, progress bars, cover for the selected title.

![Themes tour](https://raw.githubusercontent.com/vantroy/sabigoku/master/docs/media/stills.gif)

Themes: `terminal_ghost` → `phosphor` → `nord` → `tokyonight`.

---

## Features

- **TUI** (ratatui): two-pane shell, live search, detail, watchlist, Discover, toasts, context-aware status bar
- **Watchlist**: planning / watching / paused / dropped / completed; live filter; refreshes after playback
- **Discover**: AniList rankings with cover art and scores
- **Cover art**: real pixels on kitty / ghostty / WezTerm / iTerm2; halfblocks elsewhere
- **Play**: AniList metadata → streaming source → `mpv`
- **Sources**: multiple streaming sources, automatic fallback, preferred order, per-show pins
- **History**: SQLite watch history and exact resume
- **AniList sync**: Settings or `sabigoku login`; two-way background sync, or local-only
- **Settings**: live tab, saved to `~/.config/sabigoku/config.toml`; four palettes; optional transparent background
- **Updates**: boot toast on new release; `sabigoku update` upgrades in place
- **CLI**: `sabigoku <query>` runs search → pick → play

## Install

Runtime dependency: `mpv` on your `PATH`. Without it, you get a browser.

### Quick install (Linux & macOS)

```sh
curl -fsSL https://raw.githubusercontent.com/vantroy/sabigoku/master/install.sh | sh
```

Detects OS/arch, verifies against `sha256sums.txt`, installs to `~/.local/bin`.
Linux x86_64/aarch64 and macOS Apple Silicon. Intel Macs: `cargo install`.

```sh
# pin a release
curl -fsSL https://raw.githubusercontent.com/vantroy/sabigoku/master/install.sh | SABIGOKU_VERSION=0.1.1 sh

# install somewhere else
curl -fsSL https://raw.githubusercontent.com/vantroy/sabigoku/master/install.sh | BINDIR=/usr/local/bin sh
```

### AUR (Arch Linux)

```sh
paru -S sabigoku       # or: yay -S sabigoku
```

Source build of the tagged release. Recipe:
[`PKGBUILD`](https://github.com/vantroy/sabigoku/blob/master/packaging/aur/PKGBUILD).

```sh
git clone https://aur.archlinux.org/sabigoku.git
cd sabigoku && makepkg -si
```

### macOS: Homebrew

```sh
brew install vantroy/sabigoku/sabigoku
```

Prebuilt Apple Silicon binary; pulls `mpv` as a dependency. Intel Macs: `cargo install`.

### crates.io

```sh
cargo install sabigoku
```

Rust 1.90+. SQLite and TLS (rustls) are compiled in.

### Prebuilt binary

Linux tarballs are static musl. macOS ships Apple Silicon.

1. Grab the tarball from the
   [latest release](https://github.com/vantroy/sabigoku/releases/latest):

   | Machine | File |
   |---|---|
   | x86_64 Linux | `sabigoku-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz` |
   | aarch64 Linux (ARM64) | `sabigoku-vX.Y.Z-aarch64-unknown-linux-musl.tar.gz` |
   | macOS Apple Silicon | `sabigoku-vX.Y.Z-aarch64-apple-darwin.tar.gz` |

2. Verify against `sha256sums.txt` from the same release:

   ```sh
   sha256sum -c --ignore-missing sha256sums.txt
   ```

3. Extract and put it on your `PATH`:

   ```sh
   tar -xzf sabigoku-vX.Y.Z-<target>.tar.gz
   mv sabigoku ~/.local/bin/
   ```

### From source

```sh
git clone https://github.com/vantroy/sabigoku.git
cd sabigoku
cargo build --release            # -> target/release/sabigoku
```

Rust 1.90+ (edition 2024). No system SQLite or OpenSSL.

---

```sh
sabigoku                     # TUI
sabigoku frieren             # search → pick → play
sabigoku "cowboy bebop" --dub
sabigoku login               # connect AniList
sabigoku sync                # one-shot watchlist sync
sabigoku update              # self-update
sabigoku --paths             # config / data / cache locations
sabigoku --debug             # diagnostics
```

## Staying up to date

On boot, the TUI checks for a newer release (cached, at most once an hour) and
toasts if one exists. Off-switch lives in Settings. `sabigoku update` replaces
installer builds in place; AUR / Homebrew / cargo installs get the right upgrade
command instead of a side-channel overwrite.

## Development

```sh
cargo run                     # TUI
cargo run -- frieren          # CLI flow
cargo test                    # offline-safe; live network is --ignored
```

Spikes in
[`examples/`](https://github.com/vantroy/sabigoku/tree/master/examples)
de-risked the hard unknowns first:

```sh
cargo run --example spike_http
cargo run --example spike_sqlite
cargo run --example spike_concurrency
cargo run --example spike_stream
cargo run --example spike_mpv
cargo run --example spike_cover
```

**[SPIKES.md](https://github.com/vantroy/sabigoku/blob/master/SPIKES.md)** is
the annotated tour.
[`PORT.md`](https://github.com/vantroy/sabigoku/blob/master/PORT.md) and
[`docs/port/`](https://github.com/vantroy/sabigoku/tree/master/docs/port) are
product law.
[`DESIGN.md`](https://github.com/vantroy/sabigoku/blob/master/DESIGN.md) holds
layout grammar and UI contracts.

## Stack

- **TUI:** ratatui + crossterm; ratatui-image for Kitty graphics / halfblock fallback
- **Storage:** SQLite via rusqlite (`bundled`)
- **HTTP:** reqwest (blocking) over rustls
- **Concurrency:** worker threads + channels; no async runtime
- **Streaming:** multiple sources behind one provider interface, with fallback
- **Catalog:** AniList (search, Discover, metadata, covers, account sync)

## Why this exists

[zigoku](https://github.com/vantroy/zigoku) was a real app built to learn Zig.
sabigoku is the follow-up: same product in Rust, feature for feature. Same
views and store semantics; different memory model, libraries, and failure modes.
The comparison starts in
[`SPIKES.md`](https://github.com/vantroy/sabigoku/blob/master/SPIKES.md) and
continues in the port docs wherever Rust deliberately diverges.

Most of the code is AI-written under a personal agent setup. Architecture,
planning, design, and review stay human. The learning is a layer up: read the
code, question it, own the next move.

## Acknowledgements

- **[zigoku](https://github.com/vantroy/zigoku)**, the reference. Port docs cite
  it chapter and verse; its repo has the original story and inherited credits.
- **[anipy-cli](https://github.com/sdaqo/anipy-cli)** by
  [sdaqo](https://github.com/sdaqo) (GPL-3.0): zigoku's streaming support
  followed a trail opened there, reimplemented rather than copied. The GPL
  lineage carries through this port. Thank you.
- Catalogue metadata and cover art from **[AniList](https://anilist.co/)**.

## License

[GPL-3.0-or-later](https://github.com/vantroy/sabigoku/blob/master/LICENSE),
same as zigoku. Lineage runs one step further back through zigoku to
[anipy-cli](https://github.com/sdaqo/anipy-cli) (GPL-3.0), reimplemented rather
than copied. The license keeps that unambiguous.
