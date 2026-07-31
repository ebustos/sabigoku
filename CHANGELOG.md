# Changelog

All notable changes to sabigoku are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!--
Written at release time, not accumulated. Nothing lands here between tags: git
holds that record already, and a section written ticket by ticket reads like a
commit log. When a release is being cut, `git cliff --unreleased` prints a
grouped draft of everything since the last tag, that draft gets rewritten as
prose in a new `## [X.Y.Z]` section with the date, and the Cargo.toml version
and the compare link at the bottom follow.

release.yml refuses to build a tag whose section is missing or empty, and
publishes that section verbatim as the release body.
-->

## [0.1.4] - 2026-08-01

A fourth streaming source, widening the fallback pool from three to four.

### Added

- **Broader stream fallback**: a fourth source is tried automatically when
  the existing three can't resolve a stream. Softsub tracks are carried
  like the rest.

## [0.1.3] - 2026-07-31

No functional change: nothing here behaves differently from 0.1.2. This
release exists to unblock the AUR package, which failed 0.1.2's build and is
still stuck on 0.1.1; every other channel already has everything here.

### Fixed

- **AUR package build**: the AUR job runs the test suite as part of `check()`,
  and one test asserted a killed background process was gone by probing with
  `kill -0`, which also succeeds for a zombie. The build container's PID 1
  never reaps orphans, so the dead process answered the probe forever and the
  test failed every time. It now checks process state directly, treating a
  zombie as dead. AUR users get 0.1.2's self-updating, one-time zigoku import,
  and transparent background option in this release; everyone else already
  has them.

## [0.1.2] - 2026-07-31

Self-updating, a one-time zigoku import, and a transparent background option.

### Added

- **Self-updating**: a boot-time check (cached, at most once an hour) toasts
  when a newer release is out, and the Settings tab carries the current
  version plus an on/off switch (`check for updates`, on by default).
  `sabigoku update` is no longer a stub: it runs a fresh check and acts on
  the answer. A standalone install gets replaced in place; a package-managed
  one (AUR, Homebrew, cargo) gets the right upgrade command printed instead
  of a side-channel overwrite; a root-owned install is refused rather than
  silently failing.
- **Bring over your zigoku library**: sabigoku notices an existing zigoku
  library on launch and offers to bring in your shows, watch states, and
  resume points. Shows already in sabigoku are left untouched.
  Declining, or a completed import, means it won't ask again; a failed
  import changes nothing and retries on the next launch.
- **Transparent background**: every palette (`terminal_ghost`, `phosphor`,
  `nord`, `tokyonight`) now has an optional transparent background that lets
  the terminal's own opacity or blur show through; highlights, popups, and
  toasts stay opaque. Off by default; toggle it in Settings.

### Fixed

- **Browse search now pages past the first screen**: scrolling to the last
  loaded result and continuing down fetches the next page instead of
  stopping cold; a "more" marker, and a spinner while it loads, shows there's
  more to see.

## [0.1.1] - 2026-07-26

Bug fixes and four new install channels.

### Added

- **cargo, curl, AUR, and Homebrew installs**: `cargo install sabigoku` on
  crates.io, a `curl | sh` installer at the repo root that verifies the
  downloaded tarball's sha256, an AUR package (`sabigoku`, source build) for
  Arch, and a Homebrew tap (`brew install vantroy/sabigoku/sabigoku`) for
  Apple Silicon Macs. All four are fed automatically on every release.

### Fixed

- **`sabigoku <query>` works on a stock install**: the CLI now picks a source
  that can actually search, instead of binding to the primary source
  regardless of whether it supports search. `preferred_provider` still
  overrides. The TUI was never affected.
- **Watch progress can no longer run ahead of what has aired**: an airing
  show could show a full progress bar with only a few episodes out, and that
  number would get pushed to AniList. Sync now reconciles each side's
  progress against what was last agreed, instead of always taking the higher
  number.
- **A sync push can no longer overwrite an edit made elsewhere mid-sync**: if
  an entry changes on AniList between the read and the write, the push backs
  off instead of clobbering the edit and recording false agreement. A row
  that loses this race waits for the next sync run instead of pushing its
  stale value later in the same one.

### Removed

- **Intel macOS binary**: it was cross-compiled and nothing in CI could run
  it, so every shipped byte was untested. Cut rather than shipped blind.
  Apple Silicon macOS binaries are unaffected, built and smoke-tested on real
  arm64 hardware. On an Intel Mac, `cargo install sabigoku`; the installer
  says the same if run there.

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

[0.1.3]: https://github.com/vantroy/sabigoku/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/vantroy/sabigoku/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/vantroy/sabigoku/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/vantroy/sabigoku/releases/tag/v0.1.0
