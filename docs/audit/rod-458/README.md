# ROD-458 · Visual audit: zigoku vs sabigoku TUI fidelity

Side-by-side visual comparison of the two TUIs, captured under identical
conditions. sabigoku was built to `DESIGN.md`, not as a pixel-clone of zigoku, so
divergence is expected; this audit documents the delta and flags what reads as
unintentional drift vs a deliberate DESIGN choice.

## Method

Both apps driven under `tmux` at a fixed 120x40, on isolated XDG stores, same query
(`frieren`), captured as plain text grids.

- zigoku: its own `drive-tui` skill, binary `zig-out/bin/zigoku`, frozen build
  (v0.4.8 per its Settings screen).
- sabigoku: new `drive-tui` skill (added by this ticket), binary
  `target/debug/sabigoku`, at `84fb931`.

Frames live under `zigoku/` and `sabigoku/`, matched by number:

| # | View | zigoku | sabigoku |
|---|---|---|---|
| 01 | History (empty state) | ✓ | ✓ |
| 02 | Discover feed | ✓ | ✓ |
| 03 | Settings | ✓ | ✓ |
| 04 | Browse search + detail side-panel | ✓ | ✓ |
| 05 | Detail (focused) | ✓ | ✓ |
| 06 | Detail cover zoom | — | ✓ |

**tmux caveat:** covers are Kitty-graphics in both apps and tmux strips that path.
zigoku's fallback paints uniform `▀` half-blocks (blank); sabigoku's fallback
paints a dense unicode mosaic that carries per-cell color. Neither is the real
photo. A true cover comparison needs a Kitty host outside tmux (future work, see
zigoku's `capture-media` skill for the Xvfb+kitty pixel pipeline).

## Verdict

**High structural fidelity.** Same four-view model (Browse / History / Discover /
Settings), same top-bar tab strip, same season chip, same empty-state copy, same
Settings section taxonomy, same Discover rank grid, same bottom-bar hint pattern.
A user who knows one would immediately drive the other.

The divergences are small and mostly cosmetic spacing, plus a few deliberate
information-architecture choices in sabigoku's detail panel. One missing Settings
section (Updates) is a real feature gap, not a render bug.

## Findings

### F1 · Cover fallback differs (deliberate, sabigoku richer under tmux)
- zigoku: uniform `▀` half-blocks (blank rectangle) when Kitty is unavailable.
- sabigoku: dense sextant/octant mosaic carrying real per-cell color.
- Under tmux sabigoku's fallback looks better; the true test is Kitty-host pixel
  art, not captured here. **Not drift — a stronger fallback path.** Verify the
  Kitty path reaches parity on a real host (ROD-417 ratified it in ghostty).

### F2 · Header logo
- zigoku: `地獄 zigoku` (kanji + romaji wordmark).
- sabigoku: `SABIGOKU` (uppercase romaji, no kanji).
- Deliberate branding difference. Flag only if DESIGN intends a kanji wordmark.

### F3 · Settings: sabigoku is missing the Updates section
- zigoku Settings ends with an **Updates** block (`current version v0.4.8`,
  `check` toggle). sabigoku has no such section.
- Also: zigoku labels the Catalog row `enrichment sync`; sabigoku says
  `metadata refresh`. Same control, different wording.
- **Real gap / wording delta, not a render bug.** Decide whether update-check is in
  scope for sabigoku; align the label if parity is wanted.

### F4 · Detail synopsis: sabigoku truncates, zigoku expands; divider dropped
- zigoku browse side-panel shows the **full** multi-line synopsis ending
  `(Source: Crunchyroll)`, with a `───` divider between the score/genre line and
  the `28 eps · TV · …` meta row.
- sabigoku keeps the browse side-panel terse (synopsis clipped to ~2 lines with
  `…`) and drops the divider, then expands provider chips + episode grid only when
  detail is focused (frame 05).
- **Deliberate IA choice** (terse panel, rich focus). Confirm it is intended; the
  dropped divider is the one thing that reads as accidental.

### F5 · Search-result rows
- zigoku: `title … [91]` (right-aligned score, no ep count), hard-truncates long
  titles with no ellipsis (`… no Mahou Part`).
- sabigoku: `title  28ep [91]` (ep count inline before score), truncates with `…`.
- sabigoku's row carries more info and truncates more gracefully. Minor, arguably
  an improvement.

### F6 · Global horizontal offset + bottom-bar padding
- sabigoku shifts content ~1-2 columns right throughout (` ▸ ` vs `▸ `; Settings
  rows indented 2 vs flush, dividers inset vs full-width) and pads the bottom bar
  `  ▌  hjkl…` vs zigoku's ` ▌ hjkl…`.
- Consistent, so it reads as a deliberate margin, not a bug. Worth a one-line
  DESIGN note so it stays consistent.

### F7 · Season chip swap timing (behavioral)
- zigoku swaps the top-right season chip to the **selected show's** season as soon
  as a result is highlighted in the browse side-panel (`秋 2023`).
- sabigoku keeps the **current** season (`夏 2026`) in the browse side-panel and
  only swaps once detail is focused.
- Minor behavioral difference in when the chip reflects selection vs focus.

## What matches (no action)

Empty-state (copy + layout + bottom bar), Discover rank grid (`[1] Trending ·
[2] Popular · [3] Top Rated · [4] This Season`, `#N TOP [score]` cards, `TV · Nep`
chips with the same status glyphs), Settings section order and row set (Player /
Catalog / Interface / AniList Sync), toggle rendering `[████ on ████]`,
right-aligned hint column, detail field order (romaji / english / native /
status+season / `✦ [score/100] · genres` / meta line), all keybindings and
bottom-bar verbs.

## Resolution (this ticket)

| # | Decision | Change |
|---|---|---|
| F1 | Keep (sabigoku's fallback is richer) | none; pixel comparison deferred |
| F2 | Fix | wordmark `SABIGOKU` → `錆獄 sabigoku`; narrowest top-bar tier drops the ░ hairline so the label clears the right-edge dot |
| F3 | Out of scope → epic | filed **ROD-459** (packaging → release → update-check) |
| F4 | Fix | detail pane bg `surface` → base `bg`; season/year chip `fg2` → `focus`; hairline restored between score and meta in the single-column stack; synopsis flush (dropped the 2-col indent) |
| F5 | Fix (match zigoku) | eps in fixed columns as `{n} ep` / `[--]`; titles truncate instead of eps dropping. Per-track `{n} sub/dub` not available (no per-track field on the AniList-keyed row) |
| F6 | One consistent margin | browse list marker → col 2; bottom-bar `▌` gap tightened. History/Discover populated-row indents left pending (unverifiable against an empty watchlist) |
| F7 | Fix (match zigoku) | Browse season chip tracks the selected show, mirroring History |
| + | Arrow keys | `↑↓←→` now mirror `kjhl` in the global handler (Discover/History/Browse/Detail) |

Frames 07 (detail) and 08 (browse) capture the after-state. Colour changes
(F4 bg + season) are correct in code but need a real terminal to eyeball; tmux
text captures can't show them.

## Next steps

1. Pixel-level cover comparison on a Kitty host (outside tmux) for F1.
2. Confirm whether History/Discover populated-row indents should also align to
   col 2 (needs a seeded watchlist to verify).
3. Progress ROD-459 when packaging/release is on the table.
