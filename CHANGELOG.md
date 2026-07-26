# Changelog

All notable changes to sabigoku are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!--
Curated by hand. `git cliff --unreleased` prints a grouped draft from the commit
log since the last tag; copy what is worth keeping into [Unreleased], then edit
it for voice. At release, promote [Unreleased] to [X.Y.Z] with the date, bump the
version in Cargo.toml, and add the compare link at the bottom.

release.yml refuses to build a tag whose section is missing or empty, and
publishes that section verbatim as the release body.
-->

## [Unreleased]

### Added

- **`cargo install sabigoku`**: sabigoku is published on crates.io, so anyone
  with a Rust toolchain has a complete install path without downloading a
  release artifact. This is also the install route for platforms that ship no
  prebuilt binary, Intel macOS among them.

### Removed

- **Intel macOS builds are no longer published**: a release now carries linux
  x86_64, linux aarch64, and macOS arm64. The Intel mac binary could no longer
  be built on Intel hardware or run before it shipped, so it is withdrawn
  rather than published unverified. Intel Macs can still build from source.

### Fixed

- **Correcting an episode count on AniList sticks**: sync only ever raised
  progress, so lowering a wrong count on AniList did not survive. The next sync
  kept the old higher number and pushed it back over the correction. Corrections
  now land, and a local watch that has not reached AniList yet is still safe.
- **Airing shows no longer read as finished**: the lit part of a progress bar now
  stops at the last episode actually broadcast, and anything your tracked count
  claims beyond that is shaded rather than lit. A show whose count ran ahead of
  the broadcast used to paint a solid full bar while the detail pane beside it
  counted down to the next episode.
- **A resume point survives a sync**: correcting an episode count no longer
  discards the "you are partway through this episode" marker for that show.
- **`sabigoku <query>` works on a stock install**: the CLI used to bind to
  whichever source was configured and give up if that one could not search,
  which on a fresh install it never could. It now picks the first source that
  can search. A source you set by name is still honoured whenever it can search,
  and when it cannot you get a one-line note naming the one used instead.

## [0.1.0] - 2026-07-25

First tagged release, a terminal anime browser and player written in Rust.
Licensed GPL-3.0-or-later.

### Added

- **TUI shell**: browse and search, a two-pane detail view, a Discover feed,
  watchlist and watch history, live-editable settings, and an AniList connect
  flow, all in one ratatui interface. Four palettes: `terminal_ghost` (default),
  `phosphor`, `nord`, `tokyonight`.
- **Search, resolve, and play**: several streaming sources are tried in order,
  with the next one picked up automatically if one fails. Sub or dub is a saved
  preference, overridable per run with `--dub`. Playback runs through `mpv`,
  with resume positions tracked live over its IPC socket.
- **AniList sync**: connect your account and sync your list. A sync always pulls
  before it pushes, so a show that was never synced locally cannot get wiped by
  an outgoing push.
- **Cover art**: real images on terminals that support graphics (kitty, ghostty,
  WezTerm, iTerm2), halfblock rendering everywhere else.
- **Intro and outro auto-skip**: AniSkip community timestamps drive an mpv
  script that skips OP and ED automatically; configurable to skip intro only,
  outro only, both, or neither.
- **Broader stream compatibility**: streams a plain player would reject are
  handled transparently, with no extra flags or setup.
- **CLI alongside the TUI**: bare `sabigoku` opens the TUI; `sabigoku login`,
  `sabigoku sync`, and `sabigoku --version` cover account and version needs from
  the shell.
- **Diagnostics**: `--debug` (or `SABIGOKU_DEBUG=1`) turns on verbose logging, to
  stderr in CLI mode and to a rotating log file in the TUI.
- **XDG-compliant paths**: config, data, cache and runtime state each live in
  their own standard directory; `sabigoku --paths` prints exactly where.
- **Static release binaries**: linux x86_64 and aarch64 builds need nothing
  installed, not even CA certificates. macOS arm64 and x86_64 builds are also
  provided. `mpv` is the one runtime dependency, needed for playback.

### Known Issues

- **`sabigoku <query>` fails on a stock install**: the default source does not
  support search, and the CLI does not fall back to another the way the TUI
  does. The error names the `preferred_provider` values that do work; set one in
  your config, or use the TUI, which is unaffected.
- **`sabigoku update` is not implemented**: it prints a message saying so. There
  is no self-update yet.

[Unreleased]: https://github.com/vantroy/sabigoku/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/vantroy/sabigoku/releases/tag/v0.1.0
