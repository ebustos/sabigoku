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

## [0.1.0] - 2026-07-25

First tagged release, a terminal anime browser and player written in Rust.
Licensed GPL-3.0-or-later.

### Added

- **TUI shell**: browse and search, a two-pane detail view, a Discover feed,
  watchlist and watch history, live-editable settings, and an AniList connect
  flow, all in one ratatui interface. Four palettes: `terminal_ghost` (default),
  `phosphor`, `nord`, `tokyonight`.
- **Search, resolve, and play**: several streaming sources are tried in order,
  with the next one picked up automatically if one fails. Playback runs through
  `mpv`, with resume positions tracked live over its IPC socket.
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
- **XDG-compliant paths**: config, data, and cache each live in their own
  standard directory; `sabigoku --paths` prints exactly where.
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
