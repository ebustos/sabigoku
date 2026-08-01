# Sabigoku · 錆獄: Design System
## Terminal Ghost

> **Status: living spec.** sabigoku is built and released; this document describes
> a shipped app and stays the authority over it. Every color, glyph, layout rule,
> and component state here is a concrete buildable thing, and where the design had
> gaps this doc fills them with a deliberate call and labels it as such. Do not
> leave states unimplemented because "the design didn't say." Where code and doc
> disagree, that is drift: one of them is a bug, and the fix belongs in the same
> pass that found it. §10 records the settled calls with their revisit triggers;
> §11 keeps the resolved questions struck in place.
>
> **zigoku is retired (2026-07-27); sabigoku supersedes it.** Nothing here defers
> to the original any more. A deliberate divergence is a design call like any
> other: log it in §10 with a trigger. Parity is not a reason on its own.
>
> **Data rendering is governed by §8.** AniList is the catalog, search, and
> metadata brain: Browse search and the Discover feed query it directly and get
> full metadata in one response (titles, cover, score, season, genres, format,
> episode counts). Streams and episode lists come from a **multiprovider
> registry**: N concrete `StreamProvider` implementations from day one, with
> per-provider availability, absences, and pins. §8 specifies what every surface
> renders, and the degrade fallback when a field is null.
>
> **Stack:** ratatui + crossterm. Cover art via `ratatui-image` (Kitty graphics
> protocol), spiked and ratified 2026-07-17 (ROD-417; §11.2). Runtime:
> `std::thread` + mpsc, decided with the M1 cut (ROD-431; §11.1).

---

## 0. Philosophy

Sabigoku is a dark-terminal tool for someone who lives in dark terminals. The UI does
not announce itself. It does not add chrome for the sake of reassurance. It earns
attention through **color temperature, whitespace, and the one magenta cursor that
always burns.**

Rules:
- **No light theme. Ever.** Dark-only is a constraint, not a preference.
- **Color = hierarchy.** There are no font sizes. Bold, dim, italic, and color weight
  do the whole job.
- **Borders are a last resort.** Panes float in the void, divided by whitespace and
  color. Box-drawing characters appear only inside components (separator lines,
  episode grids), never as pane chrome.
- **The cover art is a hero asset.** It gets a fixed cell block and is never hidden by
  layout reflow until the terminal is too narrow to show it at all.
- **One thing is magenta at a time.** The Spectral Magenta signature is not a theme
  color. It is a pointer: it marks the single most important thing on screen right now.

---

## 1. Design Tokens

### 1.1 Palette

| Token | Hex | Usage |
|---|---|---|
| `bg.base` | `#020d06` | Terminal background. The void. Applied as cell background on every root layer. |
| `bg.surface` | `#061410` | Raised surface: currently-focused list item background, detail pane background differentiation. |
| `bg.elevated` | `#0b1f18` | Toasts, modal-ish overlays. One step above surface. Not used often. |
| `border.hair` | `#1a4030` | Hairline dividers inside components (`─`, `╌`). Not pane borders; those are whitespace. |
| `text.primary` | `#39ff6a` | All primary readable text. Titles, labels, interactive list items. Phosphor green. |
| `text.muted` | `#2a6040` | Secondary metadata: episode counts, year, genre list, synopsis body. Dim phosphor. |
| `text.dim` | `#163525` | De-emphasized rows: watched items, dropped entries, disabled states. |
| `state.focus` | `#20ffdd` | Focused / selected element. The cursor row in a list. Active pane indicator. Cyan ghost. Deliberately overdriven above `text.primary`'s luminance (0.770 vs fg-green's 0.734) so the focused row clears its neighbours instead of reading dimmer than them. Stays cyan-hued to keep the ghost identity. |
| `state.now` | `#ff2d78` | The one thing demanding attention right now. Airing status chip. Score highlight when >90. The `▌` cursor. Spectral Magenta. |
| `state.success` | `#39ff6a` | Same hex as `text.primary`; success toasts use bold primary green to signal "done." |
| `state.error` | `#ff2d78` | Error toasts. Same as `state.now`; magenta also means alarm. Context distinguishes them. |
| `state.warn` | `#e5b800` | Warning states. Used sparingly, e.g. "local DB out of sync" notices. |

### 1.2 Semantic Aliases (for implementation)

```
color.bg        = bg.base
color.surface   = bg.surface
color.chrome    = border.hair
color.fg        = text.primary
color.fg2       = text.muted
color.fg3       = text.dim
color.focus     = state.focus
color.hot       = state.now
color.warn      = state.warn
```

### 1.3 Terminal Type System

ratatui + crossterm give us: **fg/bg color, bold, dim, italic, underline, blink.**
That is the full type system. Here is how it maps to hierarchy:

| Hierarchy Level | Treatment | Example use |
|---|---|---|
| H1: Screen title | `text.primary` + bold | App name in top bar, section headers |
| H2: Item title | `text.primary` (no bold) | Anime title in list row, detail pane title |
| H2: Focused item title | `state.focus` + bold | Focused row title |
| H3: Metadata label | `text.muted` | Year, episode count, genres, score label |
| H3: Metadata value (notable) | `text.primary` | Score value when ≤ 90 |
| H3: Score ≥ 91 | `state.now` + bold | The score that earns the pointer |
| Body text | `text.muted` | Synopsis, long descriptions |
| De-emphasized / watched | `text.dim` | Completed rows in history, watched episodes |
| Status / alert | `state.now` | Kanji chips, airing indicators, the cursor |
| Command line prompt | `state.focus` | `/` and `:` prompt characters |
| Input text (live) | `text.primary` + bold | What the user is typing |
| Placeholder / hint | `text.dim` + italic | Empty search hint text |

**Bold is not decoration. Bold is promotion.** A bold element is saying "I am the
first thing you should read here." Use it once per visual unit.

**Dim is not disabled. Dim is receded.** Watched items dim; they are still navigable.
Disabled (e.g. settings toggle off) dims AND uses `text.dim` fg.

**Italic is for foreign language and inline annotation only:** English fallback gloss
for kanji chips, the synopsis ellipsis marker, loading animation frames, and the
native-language alt-title row. The italic treatment is pinned to the
*native/Japanese-script title field specifically*, not to "whichever row currently
sits in the alt position." Romaji and English never render italic, whether they are
the primary line or an alt row; native renders italic whenever it is an alt row, and
drops the treatment entirely once it becomes the primary (every primary line is bold,
never italic, regardless of which form it holds). See §8.2 for the `title_language`
preference this rule interacts with, and §10 for the rationale.

**Underline is unused.** Keybind characters in help lines and confirm prompts use
bold instead: the same "promotion" treatment as H1/H2 above.

**Blink is unused.** The `▌` status cursor was the one sanctioned use until
ROD-481: ghostty never rendered SGR blink, kitty does, and it reads as a stray
terminal cursor. Steady `state.now` now; nothing blinks.

### 1.4 Palette Selection (themes)

The §1.1 hex table is **Terminal Ghost**, the default and reference theme. Every
mock, state, and decision in this doc is authored against it. But the tokens are not
hardcoded into render code. `src/tui/theme.rs` defines a `Palette` struct (one field
per §1.2 semantic alias) and ships four concrete instances:

| Theme | Identifier | Character |
|---|---|---|
| Terminal Ghost | `terminal_ghost` | Default. The §1.1 palette verbatim. Green-on-void phosphor with cyan focus + magenta signature. |
| Phosphor | `phosphor` | Pure monochrome phosphor: `focus` and `fg` share the green hue, so bold (not color) carries focus distinction; `hot` is a complementary orange-red. |
| Nord | `nord` | Nord polar-night + snow-storm + aurora mapping. `hot` uses aurora orange (nord12) rather than nord15 purple for more urgency. **Focus distinction is hue-based, not luminance-based:** `focus` (nord8 frost) reads *dimmer* than `fg` (nord4 snow), so the focused row leans on hue shift + bold rather than out-glowing its neighbours. A deliberate trade to stay faithful to Nord's own palette relationships, not the §1.1 luminance-lift rule (ratified: §10). |
| TokyoNight | `tokyonight` | TokyoNight "night" base with a storm-bg surface tier (`bg_surface` is TN storm `#24283b`). `hot` is TN red `#f7768e`, `warn` TN yellow `#e0af68`. **Focus is a deliberate luminance lift off canonical TN:** TN's own cyan (`#7dcfff`, L≈0.56) reads *dimmer* than `fg` (`#c0caf5`, L≈0.60). Fine for an editor cursor on one glyph, wrong for a full focused row that must out-read its neighbours, and unlike Nord there is no hue rescue (both sit in the blue-lavender family). So `focus` is lifted to a brighter same-hue cyan (`#b0e8ff`, L≈0.75) to honour the §1.1 focus-clears-`fg` rule. `fg2` (`#9aa5ce`) is tuned between TN `fg_dark` and `dark5` for even `fg→fg2→fg3` spacing (`fg2`-vs-`fg3` = 2.55:1). |

The active palette is chosen by the `palette` config key (default
`"terminal_ghost"`). The app holds the active resolved `Palette` (the named theme
plus the §1.4a transparency remap); render functions reference its fields instead of
module-level constants, so a theme switch takes effect without touching component
code.

**Dark-only still holds.** All four themes are dark. "No light theme, ever" (§0) is a
constraint on every palette, not just the default: a theme is a re-hue of the same
dark system, never a light/dark toggle. **Theme-invariant rules:** one-magenta-pointer
and bold-is-promotion (§1.3) hold across every palette. The focus-clears-`fg`
luminance rule (§1.1) is *not* universal: Terminal Ghost, Phosphor, and TokyoNight
honour it (TokyoNight via a deliberate lift off canonical TN cyan; see its row), Nord
trades it for a hue-shift focus per the note above, a ratified call (§10). A new
theme must keep the two invariants; how it makes `focus` legible against `fg`
(luminance lift or hue shift) is its own call.

### 1.4a Transparent Background

`transparent_background` (config key, default `false`) is an axis orthogonal to
theme selection: any palette can run transparent. When set, the `bg` tier resolves
to the terminal's default background (`Color::Reset`) instead of the theme's
painted base, so cells that would carry the base color instead show whatever the
terminal composites there (its own opacity, blur, wallpaper). sabigoku never
handles alpha itself: the backdrop behind the window is unreadable to any terminal
app, and no standard query for the terminal's opacity exists, so there is nothing
to composite against. Leaving the cell unpainted is the entire mechanism, and it
is also why the option exists: terminals apply window opacity only to
default-background cells, so a fully painted app renders opaque even in a
transparent terminal.

Scope of the remap (first cut, ratified ROD-511):

- `bg` → `Color::Reset`. Nothing else.
- `surface` and `elevated` stay painted: focused rows, cards, and toasts remain
  opaque islands with intact §1.1 contrast over the transparent base.
- `chrome`, all fg tiers, and accents are untouched.

The base tier is the whole canvas, list and persistent detail pane alike (the
pane fills `bg.base` since ROD-458 F4, §3.1), so the app's primary surfaces go
transparent together; the opaque islands are exactly the surface/elevated
fills, not "panels" in general.

Opt-in, never default. With `bg` reset the theme's base color is ignored, and an
opaque terminal with a light default background will wreck fg contrast. That is
accepted as the user's call (they opted in); transparent mode does not force a
minimum fg tier. Dark-only (§0) is unchanged: transparency drops the paint, it
does not admit a light theme.

Users who want the painted tiers blended as well pair the toggle with their
terminal's cell-opacity control (e.g. ghostty ≥ 1.2 `background-opacity-cells`).
How explicitly painted cells behave under window opacity differs per terminal;
that is documented, not coded around.

---

## 2. Glyph / Iconography Set

All glyphs must fall inside the BMP (U+0000–U+FFFF) and be reliably present in any
terminal with a Nerd-Font-adjacent or well-populated Unicode font.

### 2.1 Status Codes

| Glyph | Token | Meaning | Color |
|---|---|---|---|
| `▌` | CURSOR | Persistent status cursor, steady | `state.now` |
| `▸` | PLAY | Playable / resume point | `state.focus` |
| `▹` | PLAY_QUEUED | In queue, not started | `text.muted` |
| `◉` | DOT_ACTIVE | Currently airing, episode just dropped | `state.now` |
| `●` | DOT_FILLED | Watched episode | `text.dim` |
| `○` | DOT_EMPTY | Unwatched episode | `text.muted` |
| `◐` | DOT_PARTIAL | Resume point (partially watched) | `state.focus` |
| `✦` | STAR_FILLED | Score decoration for top-tier entries | `state.now` |
| `·` | DOT_SEP | Metadata separator | `text.dim` |
| `─` | RULE_H | Horizontal hairline divider | `border.hair` |
| `│` | RULE_V | Vertical hairline divider (episode grid) | `border.hair` |
| `[>]` | BTN_PLAY | Play button in command context | `state.focus` |
| `[=]` | BTN_SETTINGS | Settings shortcut | `text.muted` |
| `[~]` | BTN_SYNC | Syncing indicator | `state.focus` (if active) |
| `[!]` | BTN_ERROR | Error marker | `state.now` |
| `…` | ELLIPSIS | Text truncation marker | `text.dim` |

### 2.2 Score Format

Scores are integer 0–100 from AniList. Two display forms share one colour scale:

- **Detail pane:** the full `[NN/100]` / `[NNN/100]`, with the `✦` prefix for the
  top tier. The score has a whole line to breathe.
- **List rows:** a compact `[NN]` badge: no `/100` (redundant in a tight row; the
  tier colour already reads it as a score) and **no `✦`**.

Tier colours apply to both forms (detail token shown, then list token):

- Score 91–100: `state.now` + bold; `✦` prefix in the detail pane → `✦ [97/100]` · `[97]`
- Score 76–90: `text.primary` → `[82/100]` · `[82]`
- Score 51–75: `text.muted` → `[68/100]` · `[68]`
- Score 0–50 or unscored: `text.dim` → `[--/100]` · `[--]`

### 2.3 Kanji Status Chips

These are inline text spans, not box-drawn: the bare kanji glyph(s), no brackets,
with surrounding spaces for visual separation (color alone distinguishes a chip).
The status + season/year chips render in the detail header per §4.4. The top bar
also carries a season/year chip as an *add-on beside* the view-tab strip (not a
replacement): the strip stays `state.focus`, the season chip sits two spaces after
it in `text.muted` so the two read as distinct registers (§3.4, §7.3).

| Chip | Kanji | English fallback | Color |
|---|---|---|---|
| Airing | `放映中` | AIRING | `state.now` |
| Completed | `完結` | DONE | `text.muted` |
| Not yet aired | `放映前` | SOON | `state.focus` |
| Hiatus | `休止中` | HIATUS | `state.warn` |
| Cancelled | `中止` | DROPPED | `text.dim` |
| Season year | `冬 2026` | Winter 2026 | `text.muted` (demoted from `state.focus` so two top-bar chips don't blur; §10) |

Season kanji: 春 (spring), 夏 (summer), 秋 (autumn), 冬 (winter).

The chip is the kanji text only: no box around it, no background block. Color alone
distinguishes it. The leading/trailing space is mandatory padding.

### 2.4 Watchlist Status Labels

| Status | Glyph + text | Color |
|---|---|---|
| Watching | `▸ watching` | `state.focus` |
| Completed | `● complete` | `text.muted` |
| Planning | `○ planning` | `text.muted` |
| Paused | `◐ paused` | `state.focus` + dim |
| Dropped | `· dropped` | `text.dim` |

This is the canonical status-label spec (group headers keep these colors). In a
**list row**, the watching/paused glyph color is overridden by the §4.1 selection
rule: the status glyph reads `text.muted` when unselected and only becomes
`state.focus` when the row is selected **and** the list pane has focus.
`state.focus` is the cursor's color, not a status color.

---

## 3. Layout Grammar

### 3.1 The Borderless Float System

Panes are separated by:
1. **Whitespace:** a 2-cell gap between the list column and the detail column.
2. **Color differentiation:** ~~the detail pane background is `bg.surface` where the
   list column is `bg.base`. The boundary is visible without a line.~~ Retired by
   the zigoku parity pass (ROD-458 F4): the pane fills `bg.base` like the list, and
   the gap plus alignment alone carry the boundary. In §1.4a transparent mode both
   regions accordingly ride the terminal backdrop together.
3. **Content alignment:** list content is left-aligned; detail content uses the
   leftmost cell of its column as the margin anchor.

No outer border. No pane-chrome box-drawing. The app fills the terminal window edge
to edge with `bg.base`, and content floats within it.

### 3.2 Column Structure: Browse / History / Detail (shared layout)

```
┌────────────────── TERMINAL WIDTH ──────────────────┐
│ 1-cell margin                                       │
│  TOP BAR             (full width, 1 row)            │
│ 1-cell spacer                                       │
│  [LIST COLUMN]  2-cell gap  [DETAIL COLUMN]         │
│                                                     │
│  list col: 38% of terminal width, min 30 cols       │
│  detail col: remaining width minus gap              │
│                                                     │
│  BOTTOM BAR / CMD LINE  (full width, 1 row)         │
│ 1-cell margin (implicit: bottom of terminal)       │
└─────────────────────────────────────────────────────┘
```

Column widths flex with terminal resize. This two-pane geometry is shared by Browse
and History. Cover art sizing in the detail pane is governed by the **effective
column width** (`detail_w`), not terminal width, so it scales correctly in both the
persistent pane and the full-screen zoom:

| `detail_w` (effective col width) | Cover width | Cover height |
|---|---|---|
| ≥ 40 cols | `20 cols` (§3.3 hard cap) | geometry-derived (poster aspect), capped at 28 rows |
| 25–39 cols | `14 cols` | geometry-derived, capped at 20 rows |
| < 25 cols | hidden | - |

Width is fixed by tier; **height** derives from the terminal's reported pixel
geometry so the poster stays poster-shaped, capped at the aesthetic max above. In
the single-column layout that height is *additionally* bounded so it can't starve
the episode grid; see §3.3 "Cover height yields to the grid."

Below 60 cols terminal width, collapse to single-column list only (no detail pane).

The split formula is implemented as `pane_split(w)` in `src/tui/layout.rs`, a shared
helper that returns `{ list_w, detail_x, detail_w }`. Used identically by Browse and
History so the geometry is identical across both surfaces. This helper is the single
source of truth; do not re-derive these numbers elsewhere.

```
list_w   = max(30, w * 38 / 100)
detail_x = 2 + list_w + 2          // 2-cell left margin + list + 2-cell gap
detail_w = w - detail_x - 1
```

Sample widths:

| Terminal width | list_w | detail_w | cover tier |
|---|---|---|---|
| 80 cols | 30 | ≈45 | 20-col cover (detail_w ≥ 40) |
| 100 cols | 38 | ≈57 | 20-col cover |
| 120 cols | 45 | ≈70 | 20-col cover |
| 160 cols | 60 | ≈95 | 20-col cover |

**Named threshold constants:**

| Constant | Home | Value | Meaning |
|---|---|---|---|
| `PANE_SPLIT_MIN` | `src/tui/layout.rs` | `60` | Browse and History split to two panes at or above this width; below it, single-column list only. Also the single detail-surface threshold: at or above this width, a focused detail pane renders its interactive episode grid **in-pane** and `Enter` plays from it; `Space` still promotes to the roomier full-screen zoom at any width. There is no separate mid-tier zoom gate (ratified: §10). |
| `DETAIL_TWO_COL_MIN` | `src/tui/view/detail.rs` | `100` | Gates the two-internal-column split (§5.3) wherever a detail pane is drawn, keyed to that pane's **own width**, not the terminal. Governs both the History persistent two-pane (engages at `term ≥ 168`, once the 38% list is subtracted) and the full-screen zoom's internal split (engages at `term ≥ 102`, since the zoom's pane is `term - 2`). It gates layout only; the metadata is one compact line at every width (§5.3a). |

`PANE_SPLIT_MIN = 60` is where the in-pane grid begins: grid columns
`≈ detail_w / 5` give a narrow but real ≈ 5 columns at 60 cols, growing to ≈ 8
usable columns at 100 cols, adequate for the 12–26 ep majority. The zoom earns its
keep for long-runners at 160+ cols (≈ 19 columns, §5.4a).

### 3.3 Cover Art Block

The cover art occupies a fixed region at the top of the detail column, left-aligned
to the column origin. No border around it. Padding: 1 cell above, 0 cells left
(flush to column), 1 cell below before the metadata section.

**Kitty protocol path:** render the cover image via `ratatui-image` into the fixed
cell block. The image is aspect-ratio cropped to fill the block (no letterboxing;
the crop is intentional, like a book cover). This pipeline is spiked and ratified
(ROD-417, 2026-07-17; §11.2): protocol detection, pixel-geometry queries, crop, and
resize redraw all validated, so the sizing rules here are trusted.

**Half-block fallback:** when Kitty graphics are unavailable, render the cover into
the cell block with `▄`/`▀` half-block cells (`ratatui-image`'s halfblocks
protocol). This is not great, but it preserves the visual weight of the cover
region.

**Loading state:** render the cover block with `bg.surface` fill and a centered
loading spinner (§4.8).

**Cover sizing rule.** Select the cover tier from the **effective column width**
(`detail_w` for the persistent pane; full canvas width minus margins for the
full-screen zoom, §5.3), not terminal width. Hard cap: `cover_w` never exceeds
20 cols (§0: "ghostly, not gaudy"). The tiers from §3.2 apply. Passing terminal
width unchanged to the cover draw is incorrect in the persistent pane context.

**Cover height yields to the grid.** In the single-column detail layout the cover,
header, synopsis, and episode grid share one vertical column, so a tall cover can
crowd the grid out. Worst case at a 35-row terminal (pane height 32): a terminal
reporting *no* pixel geometry makes the cover fall back to its full 28-row aesthetic
cap and leaves the grid no rows. The contract: **the episode grid always keeps ≥ 2
visible rows for a ≥ 28-episode show.** Two complementary caps enforce it, both in
`src/tui/view/detail.rs` (the single source of truth; do not re-derive these numbers
elsewhere):

- `cover_height_cap(h)` bounds the cover so `cover + worst-case header + a 2-line
  synopsis + the grid's spacer + 2 grid rows` always fit (`cover_reserve` rows
  reserved below the cover). Below `min_cover_rows` (6) the squashed poster is
  dropped entirely rather than rendered as a sliver.
- `synopsis_cap(remaining)` then clamps the synopsis to leave the grid its 2 rows,
  appending the italic dim `…` truncation marker (§1.3).

The two-column zoom (§5.3) and the History preview stack put the cover in a column
that does **not** contain the grid, so they are exempt: the cover draw takes a
`max_h_override` that only the single-column path supplies.

### 3.4 Top Bar

Single row. Full terminal width. Content:

```
  SABIGOKU  ░  [B]rowse · [H]istory · [D]iscover · [S]ettings  冬 2026
```

- App name: `text.primary` + bold. Always visible, never interactive.
- `░` separator: `border.hair`.
- View tab strip: a persistent four-tab strip naming every view, with the active one
  highlighted; the same passive idiom as the §3.8 axis bar. Each tab brackets its
  view-switch key letter (`[B]rowse · [H]istory · [D]iscover · [S]ettings`), so the
  strip both shows *where you are* and teaches the keys. It is **passive**: no tab
  focus model, no `j`/`k` into it; the bracketed letters fire the existing
  normal-mode binds from anywhere (§6.1/§7.2). Styling: active tab `[X]` in
  `state.focus`, label in `state.focus` + bold; inactive `[X]` in `text.muted`,
  label in `text.muted`; separator `·` in `text.dim`. The detail zoom is not a tab
  destination; it highlights the `detail_origin` tab (`[B]rowse` / `[H]istory` /
  `[D]iscover`), so the strip still reads "where you came from."
- Season/year kanji chip: an add-on two cells after the strip, in `text.muted` so it
  reads as metadata distinct from the cyan strip (and never competes with the cyan
  `·` at the right edge). Content: the currently selected show's season+year when a
  row is selected and both are known; otherwise the current real-world cour from the
  system clock (AniList's season boundaries: 冬 Dec–Feb, 春 Mar–May, 夏 Jun–Aug, 秋
  Sep–Nov, with December rolled into next year's Winter, so it agrees with the show
  chips). The detail zoom is the exception: committed to one show, it shows only
  that show's season with no cour fallback. Discover tracks the selected card's
  season+year (absent if null; no cour fallback; §3.8/§7.3); Settings shows no chip.
  The chip drops first under width pressure (below w ≈ 78).
- Right-aligned: active pane indicator (a `·` in `state.focus` color to mark which
  pane has keyboard focus, list or detail).

**Width degradation:** w ≥ 78: full strip + season chip · 66 ≤ w < 78: full strip,
no chip · 42 ≤ w < 66: abbreviated strip `[B] · [H] · [D] · [S]` (active `[X]` =
focus + bold, inactive = dim), no chip · w < 42: single active label fallback. The
abbreviated strip and the right `·` always survive. (These breakpoints sit 2 cols
above the geometry a 6-cell app name would allow: SABIGOKU is 8 cells.)

No search bar. No breadcrumbs. The strip is read-only state (it displays where you
are; it does not accept input); the top bar stays read-only context, not UI.

*(The full-screen view mockups in §5 render the top bar in shorthand, the single
active label, e.g. `SABIGOKU ░ Watchlist`, to keep those wide diagrams legible.
This section is the canonical top-bar spec.)*

### 3.5 Bottom Bar / Command Line

Single row. Full terminal width. This row does triple duty:

**State 1: Idle help line:**
```
  ▌  hjkl · / search · : command · q quit
```
- `▌` in `state.now`, steady.
- Text in `text.dim`.
- Keybind characters (h, j, k, l, /, :, q) in `text.muted` + bold (§1.3).

**State 2: Search active (triggered by `/`):**
```
  /  frieren_                                   [12 results]
```
- `/` prompt: `state.focus` + bold.
- Typed query: `text.primary` + bold.
- `_` cursor: `state.focus`.
- Result count (right-aligned): `text.muted`.
- List filters live above as characters are typed. No submit required.
- `Esc` returns to idle help line and clears the filter.
- `Enter` locks the search and moves focus to the list.

**State 3: Command active (triggered by `:`):**
```
  :  _
```
- `:` prompt: `state.now` + bold.
- Input: `text.primary` + bold.
- Recognized commands: §6.3.
- Unknown command: flash bottom bar `state.error` for 800ms, return to idle.

A fourth in-practice state, the hard-delete **confirm prompt**, is specified in
§4.2/§6.5; it shares the marker-replacement rule below.

### 3.6 Internal Dividers

The only box-drawing used inside content areas:

- `─` horizontal rules between sections in the detail pane (`border.hair`).
- `│` vertical separators in the episode grid only.
- `╌` dashed rules for "loading more" indicators.

No other box-drawing anywhere.

### 3.7 Margin and Padding Rules

| Location | Rule |
|---|---|
| Left edge of content | 2-cell left margin from terminal edge |
| Top bar / bottom bar | 1-cell left/right padding within the bar |
| List rows | 1-cell left indent, 1-cell right padding |
| Detail pane left edge | 2-cell gap from list column right edge |
| Detail pane content | 0-cell additional indent (flush to column) |
| Cover art top | 1 blank row above |
| Cover art bottom | 1 blank row below (before metadata) |
| Metadata sections | 1 blank row between sections |
| Synopsis | 2-cell left indent, word-wrapped to column width |

### 3.8 Discover: Layout Grammar

Discover is a **full-canvas, single-pane** view (`active_view = .discover`). There
is no list/detail split and no `active_pane` semantics; the entire terminal canvas
is one scrollable card grid.

**Row structure (top to bottom):**

```
┌────────────────── TERMINAL WIDTH ──────────────────┐
│  TOP BAR               (1 row, §3.4)               │
│  spacer                (1 row)                      │
│  AXIS BAR              (1 row)                      │
│  spacer                (1 row)                      │
│  CARD GRID             (all remaining rows, scroll) │
│  BOTTOM BAR            (1 row, §3.5)               │
└─────────────────────────────────────────────────────┘
```

Chrome overhead: **5 rows.** The card grid receives every row between the second
spacer and the bottom bar.

**Card-grid geometry.** Two width-keyed tiers, consistent with the §3.2/§3.3
cover-size breakpoints:

| Terminal width | Cover cell | Slot | Column formula |
|---|---|---|---|
| ≥ 80 cols | 20 × `cover_h` | 22 × (`cover_h`+4) | `max(1, (w - 2) / 22)` |
| < 80 cols | 14 × `cover_h` | 16 × (`cover_h`+4) | `max(1, (w - 2) / 16)` |

`w - 2` removes the 2-cell left margin (§3.7). Each card occupies one slot
(`slot_w × slot_h`). `slot_h = cover_h + 4`: three meta rows (rank+badge+score,
title, format+genre-glyphs) plus one gap row. Rows visible per frame =
`(content_h - 2) / slot_h`, where `content_h - 2` removes the axis bar row and
its spacer from the content height.

**Adaptive cover height.** `cover_h` is derived from the terminal's reported cell
pixel dimensions so a ~2:3 AniList poster fills the card width rather than
pillarboxing inside a too-short box. For a 20-col card on a terminal reporting
10×22-px cells, `cover_h ≈ 13` and `slot_h = 17` (measured for real in ROD-417:
ghostty reports 9×20-px cells and lands `cover_h = 13`). When cell pixels are unreported
(tmux, headless, SSH setups that don't answer the pixel metric) the height falls
back to fixed values, 7 for the large tier and 5 for the small, which are always
the minimum (the adaptive height never shrinks below them). The trade: taller
covers mean fewer card-rows above the fold, offset by fuller poster art.

**Axis bar.** One row at y = 2 (after top bar + spacer), left margin 2 cells.
Content: `[1] Trending · [2] Popular · [3] Top Rated · [4] This Season`. Each axis
is prefixed with its `1`–`4` direct-select key so the bar teaches its own bindings
in place. The four axes are AniList's four independent ranking axes:

| Axis | AniList sort | Notes |
|---|---|---|
| Trending | `TRENDING_DESC` | Default axis. AniList's own hot-right-now signal. |
| Popular | `POPULARITY_DESC` | All-time member-list count. |
| Top Rated | `SCORE_DESC` | Score-ranked. |
| This Season | `POPULARITY_DESC` + `season`/`seasonYear` filter (current cour) | A season-scoped view. |

| State | Token | Modifier |
|---|---|---|
| Active axis label | `state.focus` | bold |
| Inactive axis labels | `text.muted` | - |
| Active axis `[N]` key | `state.focus` | - (lifts with the label so the entry reads as a unit) |
| Inactive axis `[N]` keys | `text.muted` | - (legible; it's the binding being taught, and `text.dim` buries it) |
| Separator `·` dots | `text.dim` | - |

The bar is **passive**: there is no "axis bar focus." The `[`/`]` cycle keys and
`1`–`4` direct-select keys drive the active axis regardless of the grid cursor
position; the inline `[N]` annotations make those direct-select keys discoverable
without a focus model. An axis change clears results, shows the loading state,
refetches per the axis→query mapping (§8.6), and resets cursor and scroll.

`This Season` suppresses the card-level `NEW` badge (item 3 below): every card on
that axis is already this-cour by construction, so the badge would fire on every
card and stop meaning anything. `TOP` (rank #1) is unaffected; it tracks position,
not recency, and stays meaningful on every axis.

**Card anatomy.** Each card occupies one slot, rendered top-to-bottom:

1. **Cover block** (`cover_w × cover_h` cells). Kitty image when art is available;
   half-block fallback for terminals without Kitty support. Placeholder while art is
   loading or unavailable: `bg.surface` fill + rank label `#N` centered in
   `text.dim`. The fill is the only `bg.surface`-elevated element in the grid;
   real cover art replaces it once available.
2. **Selection marker.** `▸` in the **left gutter at `x-1`** (one column left of the
   card's content origin) on the **rank row** (`y + cover_h`), `state.focus`,
   text-on-base. No box border, no background band. The marker does not touch the
   cover cell, so cover art is never masked or composited. Combined selection cue:
   `▸` in the gutter + title in `state.focus` + bold.
3. **Rank + badge + score row (row 0).** `#N` in `text.primary`, left-anchored.
   At most one badge follows the rank:
   - Rank #1: `TOP` in `state.now` + bold.
   - Current-cour release (exclusive with `TOP`): `NEW` in `state.focus` + bold.
   Both badges are **derived render-side** from rank index and season/year; they
   are not payload fields.
   A **score badge** `[NN]` / `[--]` is right-anchored at the cover edge on the same
   row, never colliding with the left-anchored rank (they grow from opposite ends).
   Tier colour per §2.2 with one exception: the 91+ tier is capped at `text.primary`
   on cards. `state.now` is reserved for the `TOP` rank pointer (§0
   one-magenta-at-a-time), so a top-scored #1 card does not double-paint both `TOP`
   and the badge in hot+bold. `[--]` in `text.dim` for null scores.
4. **Title row (row 1).** The resolved primary title (`title_language`, §8.2) in
   `text.primary` (unselected) or `state.focus` + bold (selected). Clipped to
   `cover_w` columns with `…` (§2.1).
5. **Format + genre row (row 2).** `format` in `text.muted`, left-anchored:
   `TV`, `Movie`, `OVA`, `ONA`, `Spec`, `Music` (abbreviated to fit the 14-col small
   tier). Episode count follows as `· Nep` when `episodes` is non-null and the
   format isn't `Movie` (a movie's episode count of 1 is redundant with the format
   label): `TV · 24ep`. An airing show with an unannounced total renders `TV · ??ep`
   rather than guessing. `format` itself null or unmapped: `—` in `text.dim` (whole
   field absent, not a partial render). Up to two **genre glyphs** are
   right-anchored at the cover edge on the same row, in `text.dim`, single-space
   separated: ambient glyph texture rather than a label; the full genre list lives
   in the zoom detail pane. Monochrome BMP symbols (not emoji) so they render
   deterministically over tmux/Kitty/SSH. The single space is what keeps the pair
   legible; `text.dim` keeps them as texture. Absent for genre-unmapped cards.
   Vocabulary: §3.8a.
6. **Gap row (row 3).** Empty; gives the grid visual breathing room between card rows.
7. **Peek row (when space allows).** After the last full card row, any leftover
   vertical band (≥ 3 rows tall) renders the tops of the **next card-row's covers**,
   clipped to the band height. This signals "more content below" instead of dead
   space. No meta rows appear in the peek band; covers only. The load-more footer
   yields to the peek row and renders only when the peek band is absent.

**Card token summary:**

| Element | Token | Modifier |
|---|---|---|
| Cover placeholder fill | `bg.surface` | - |
| Cover placeholder rank `#N` | `text.dim` | centered in cover block |
| Selection `▸` | `state.focus` | rank row, left gutter (`x-1`) |
| Rank `#N` (metadata row) | `text.primary` | - |
| `TOP` badge | `state.now` | bold |
| `NEW` badge | `state.focus` | bold |
| Score badge `[NN]` | §2.2 tier colour; 91+ capped at `text.primary` on cards | right-anchored at cover edge, rank row |
| Score badge `[--]` (null) | `text.dim` | right-anchored at cover edge, rank row |
| Title (unselected) | `text.primary` | clipped with `…` |
| Title (selected) | `state.focus` | bold, clipped with `…` |
| Format + episode count | `text.muted` | `TV · 24ep` / `Movie` / `TV · ??ep` |
| Format absent | `text.dim` | `—` placeholder |
| Genre glyphs (≤ 2) | `text.dim` | right-anchored at cover edge, format row; single-space separated |

**Grid states.** When the results array is empty, one of three states renders
centered in the card-grid region:

| State | Render | Token |
|---|---|---|
| Initial load / axis refetch | `⠋ loading feed…` | `state.focus`; escalates to `state.now` + `taking a moment…` after >3 s (§4.8 slow rule) |
| Empty (no entries returned) | `no entries` | `text.muted` + italic |
| Error / offline | `[!] can't reach the feed` (heading) · `check your connection` (sub-line) | `state.now` + bold · `text.muted` + italic |

The loading state uses the §4.8 braille spinner and `is_slow_path()` escalation. The
error state is persistent: the feed is unreachable, not transiently failed. `[`/`]`/
`1`–`4` retry by refetching the active axis; the error clears on the first
successful response. This is the §8.5 unreachable pattern applied to the Discover
feed.

When results are already on screen and a next page is in flight, a load-more footer
renders below the last visible card row:

| State | Render | Token |
|---|---|---|
| Fetching next page | `⠋ loading more…` | `text.muted` + italic |
| Feed exhausted (last card in view) | `all entries loaded` | `text.dim` |

**Season chip and `·` dot.**

The season chip (§3.4) tracks the **selected card** in the Discover grid: the
kanji+year chip appears in the top bar for the cursor position as soon as the card
is on screen. AniList's feed response arrives fully enriched (§8.6), so the chip's
data is available the moment the card renders. When the selected card genuinely has
no season data (AniList returned null; rare, mostly unannounced titles) the chip is
absent. There is no cour fallback: the grid has no ambient single-season context,
and a misleading ambient chip would collide with the "selected show's season"
meaning it carries in Browse/History. The detail zoom
(`detail_origin = .discover`) follows the same rule.

The `·` pane-focus dot is always `state.focus` in Discover. The view is
single-pane: there is no list/detail split and therefore no dim state.

### 3.8a Genre Glyph Vocabulary

The genre glyph map covers AniList's fixed genre vocabulary. This table is the
canonical source; the implementation in `src/tui/view/discover.rs` (`GENRE_GLYPHS`
array) must match it exactly; edit both together (drift is rot). All glyphs are
monochrome BMP codepoints so they render predictably in any terminal font without
colour or width ambiguity. Genres not listed here map to no glyph and are silently
skipped. A card shows at most two glyphs (the first two mappable genres in AniList's
returned order).

| AniList genre | Glyph | Unicode | Name |
|---|---|---|---|
| Action | ⚔ | U+2694 | Crossed swords |
| Adventure | ⚑ | U+2691 | Flag |
| Comedy | ☺ | U+263A | Smiling face |
| Drama | ◆ | U+25C6 | Diamond |
| Ecchi | ♨ | U+2668 | Hot springs |
| Fantasy | ⚜ | U+269C | Fleur-de-lis |
| Horror | ☠ | U+2620 | Skull |
| Mahou Shoujo | ✿ | U+273F | Flower |
| Mecha | ⚙ | U+2699 | Gear |
| Music | ♪ | U+266A | Music note |
| Mystery | ◈ | U+25C8 | Diamond-in-diamond |
| Psychological | ◐ | U+25D0 | Half circle |
| Romance | ♥ | U+2665 | Heart |
| Sci-Fi | ⬡ | U+2B21 | Hexagon |
| Slice of Life | ❖ | U+2756 | Ornament |
| Sports | ◎ | U+25CE | Bullseye |
| Supernatural | ☽ | U+263D | Crescent moon |
| Thriller | ↯ | U+21AF | Lightning |

The `◆`/`◈`/`❖` and `◐`/`◎` shapes are legible at `text.dim` (ratified ROD-509,
§11.4). They separate at silhouette level (fill, mass, outline weight), which
dimming preserves, never by internal fine detail, which it erases; the single
separator space is what stops two adjacent glyphs merging into one shape
(ROD-247). A glyph added here inherits both rules and the test that established
them: render it at `fg3` on `bg.base` beside its nearest neighbour in this
table (§11.4's conditions). A codepoint chart is not the test.

---

## 4. Component States

### 4.1 List Row

A list row is 1 cell tall. Content: `[STATUS_GLYPH] [TITLE…truncated] [SCORE]`

Score is right-aligned within the list column as the compact `[NN]` badge (≤5 cols,
§2.2), right-anchored against the *pane* edge, not a fixed column, so it survives
the split list pane. Title truncates with `…` if it would overflow the score field.
In Browse an episode-count field may sit to the score's left when the pane is wide;
priority is **title > score > eps**, so a tight pane drops the count first and never
squeezes the title to keep it (§4.3).

| State | Background | Title color | Score color | Left glyph |
|---|---|---|---|---|
| Default | `bg.base` | `text.primary` | per score rules | none / `·` dim |
| Selected, list focused | `bg.surface` | `state.focus` + bold | per score rules (focus overrides nothing) | `▸` in `state.focus` |
| Selected, list **unfocused** (detail pane active) | `bg.base` | `state.focus` (no bold) | per score rules | `▸` in `state.focus` dim |
| Watched / completed | `bg.base` | `text.dim` | `text.dim` | `●` in `text.dim` |
| Currently watching (unselected) | `bg.base` | `text.primary` | per score rules | `▸` in `text.muted` |
| Paused (unselected) | `bg.base` | `text.primary` | per score rules | `◐` in `text.muted` + dim |
| Airing (live) | `bg.base` | `text.primary` | per score rules | `◉` in `state.now` |
| Search non-match (filtered out) | not rendered | - | - | - |

The selection indicator is the row's background shift + bold title + `▸`. There is
no full-row color highlight. The background shift (`bg.base` → `bg.surface`) is
subtle but consistent.

**Selection is focus-aware.** `state.focus` (cyan) is reserved for the selection
affordance, and the affordance is earned only when the row is selected **and its
list pane holds keyboard focus**. When the detail pane takes focus the selected row
steps down: the `bg.surface` band drops back to `bg.base`, the `▸` dims, and the
title loses its bold, so the active pane is unmistakable (the symmetric step-up is
the detail/grid lighting). This is why a non-selection status color (the `◐`
watching glyph) must NOT borrow `state.focus`: an unselected `watching` row in cyan
would impersonate the cursor. Watching/paused/completed/planning glyphs use
`text.muted`; `dropped` uses `text.dim`; only the selected, list-focused row gets
`state.focus`. Applies identically to Browse and History (the two-pane list
grammar, §7.3).

### 4.2 Bottom Command Line (all four states)

Fully specified in §3.5. Component summary:

| State | Trigger | Left indicator | Prompt color | Input color |
|---|---|---|---|---|
| Idle help | default | `▌` steady `state.now` | - | `text.dim` |
| Search | `/` | `/` static | `state.focus` + bold | `text.primary` + bold |
| Command | `:` | `:` static | `state.now` + bold | `text.primary` + bold |
| Confirm (delete) | `X` (History list) | `[!]` static `state.now` (▌ suppressed) | static text `text.muted` | title `text.primary` + bold |

When search or command is active, the `▌` is replaced; the prompt character
takes its visual position. The confirm state (§6.5) is a fourth in-practice mode
with the same replacement rule, driven by `confirm_delete` rather than `input_mode`.

**Confirm prompt (80-col):**

```
[!] delete "Sousou no Frieren"? episode history gone · y confirm · esc cancel
```

`[!]` is `state.now`; static text (`delete "`, `"? episode history gone`, the
`y confirm` / `esc cancel` labels) is `text.muted`; the `·` separators are
`text.dim`; the show title is `text.primary` + bold, `…`-truncated against a
~48-col fixed tail so the y/esc hints never scroll off. The `y`/`esc` keybind
characters use the standard hot/fg2 + bold keybind-hint treatment (§1.3).

### 4.3 Score Display

Full spec in §2.2. In a list row, the score is the compact `[NN]` badge (≤5 cols,
no `/100`, no `✦`), right-anchored against the list pane's right edge. Geometry is
pane-relative (the dominant Browse layout is the ~38%-width split list pane), and an
episode-count field may share the meta zone to its left on a wide pane (title >
score > eps). In the detail pane, score is rendered larger by adding whitespace and
the `✦` prefix for top-tier entries.

Detail pane score line format:
```
  ✦ [97/100]  · Action · Adventure · Drama
```
- `✦` + score: `state.now` + bold if ≥ 91.
- `·` separators: `text.dim`.
- Genres: `text.muted`.

### 4.4 Status Chip (Kanji) and the Detail Header

Inline spans, no border; color carries the meaning (§2.3). The detail header stacks
**the resolved primary title** → its two alt-title rows → **chips row** →
score+genres, so the chips render on their own row beneath however many alt-title
lines are present rather than trailing the title inline (the alt-titles claim the
title's row). Primary resolution is the `title_language` preference (default
`romaji`, §8.2); the two alt rows are the remaining forms in `romaji → english →
native` order minus whichever form is primary, each omitted when null or byte-equal
to the primary (`draw_alt_titles`). Styling is keyed to the *field*, not the row
position: the native form renders italic whenever it lands in an alt row, romaji and
English alt rows are always plain `fg2`, never italic (§1.3). On that dedicated row
the chips sit **flush at column 0**, aligned with the title stack; no leading
indent. Up to four segments share the row, in fixed order, each pair separated by
two spaces: **status** chip, **season+year** chip (§2.3), an **airing countdown**
(releasing shows only), and a **non-JP origin marker** (non-JP shows only) trailing
last.

```
Sousou no Frieren
Frieren: Beyond Journey's End
葬送のフリーレン
完結  秋 2023
✦ [93/100] · Adventure · Drama · Fantasy
```

Shown above under the default `romaji` preference: romaji bold, then English plain
and native italic. Under `english`, the same three strings reorder: English bold
primary, then romaji (plain) and native (italic) as alts. Under `native`, native
leads bold and is **not** italic; italic only marks it when it is an alt, never
when it is primary.

When a title carries no alt-title lines, the chips still take their own row for a
consistent header rhythm. Each segment is omitted entirely when its field is absent
(no empty span); the row itself is skipped only when every segment is absent.

**Airing countdown.** A third segment, releasing shows only, sourced from AniList
`nextAiringEpisode{episode airingAt timeUntilAiring}`:

```
放映中  春 2026  Ep14 · 3d
```

Format: `Ep{episode} · {countdown}`, rendered in `state.now`; it shares the airing
chip's own register, since both mark "act on this right now." The countdown
collapses to a single coarsest unit, never combined (`3d`, not `3d 4h`): `≥ 1 day`
renders `Nd`, `< 1 day` renders `Nh`, `< 1 hour` renders `Nm`. **Persist the
absolute, not the relative:** store `airingAt` (unix seconds) plus the episode
number, and recompute the countdown from `state.now` at render time.
`timeUntilAiring` is only correct at fetch time and drifts the instant the process
keeps running, so persisting it verbatim would go stale; the absolute timestamp
survives a restart and stays correct indefinitely. If the recomputed countdown has
already lapsed (a stale `nextAiringEpisode` in the window between the real airing
and the next metadata refresh) the segment is omitted rather than rendered negative
or as a bare "airing": a wrong countdown is worse than no countdown (§10 logs this
call).

**Non-JP origin marker.** A low-noise trailing segment surfacing AniList
`countryOfOrigin` whenever it is **not** `JP`; a donghua/aeni show like *Mo Tian
Ji* (CN) earns a marker; the common Japanese-origin case shows nothing:

```
完結  夏 2016  CN
```

Rendered as the bare two-letter AniList country code in `text.dim`, the dimmest
tier on the row, deliberately, and last in segment order: it is the least
time-sensitive fact here, so it reads after the live status/season/countdown
information rather than competing with it. No flag glyph: an actual flag emoji is a
Supplementary-Plane regional-indicator pair, outside the §2 "glyphs must fall
inside the BMP" contract, so a plain text country code is the only form that
renders deterministically across every terminal this app targets (§10 logs this
call).

### 4.5 Progress Bar

Used in History/Watchlist view only. Represents episode progress.

Format: `[████████░░░░░░░░]  8 / 28 eps`

- Filled cells: **selection-aware** (see below); `state.focus` only on the cursor
  bar, otherwise the per-status color.
- Empty cells: `border.hair`.
- `█` for watched, `░` for empty, `▓` for watched-past-broadcast, `·` for
  not-yet-aired. `▓`, `░` and `·` are all `border.hair`, so the **lit `█` run
  always stops at the broadcast edge**: a `▓` claim has to read as unlit, or the
  row scans as a full bar again and the whole point is lost. The `◐` resume
  marker is the one deliberate exception and takes the fill colour wherever it
  lands, edge or no edge.
- Bar width: 16 chars minimum, scales to available space with a max of 24 chars.
- Episode fraction text: `text.muted` on the cursor bar, else `text.dim`.
- Resume point: a `◐` injected at the resume position within the bar, e.g.
  `[████◐░░░░░░░░░░░]` where `◐` is at episode 5 of 28. It outranks every other
  glyph: it is a real watch, and the bar must never hide one.
- **Broadcast edge.** On an airing show the cells past the aired count
  (`aired_count`, `nextAiringEpisode - 1`) render in their own register, so the
  bar separates "not watched" from "not out yet":
  `[███·············]  3 / 14 eps` is three of the three episodes out.
- **Progress past the edge** renders `▓` rather than being clipped:
  `[████▓▓▓▓▓▓▓▓▓▓▓▓]  14 / 14 eps` on a season with four aired says "your
  tracker claims more than has been broadcast" without erasing a watch. **The
  fill is never capped at the aired count.** `nextAiringEpisode` comes from
  cached enrichment and goes stale for a full TTL, which is exactly the window a
  weekly viewer lives in, so a cap would hide the episode they watched three
  hours after it aired. `▓` degrades to a mild over-claim for a day instead.
- **This is deliberately not the §4.6 grid rule.** `expected_episode_count`
  (02 §4, ROD-359) caps the grid at `min(aired, total)`, so the grid on that
  same show draws four cells while the bar spans fourteen. The two answer
  different questions: a grid cell is a thing you can press Enter on, and
  offering one for an episode that does not exist is a broken affordance,
  whereas the bar reports a watch the user already has. Capping a count of
  things-you-can-do is correct; capping a record of what happened is data loss.
  A future reader diffing the two must not "fix" them into agreement.
- Edge and fill round identically and carry the same one-cell minimum.
  Asymmetry either way puts the edge ahead of a fill reaching the same
  episode, so a viewer caught up on everything broadcast gets a phantom `░`. The
  one-cell minimum is what gives a lone aired or watched episode a cell of its own
  on a long season, where the quotient truncates to zero.

The fill color is **selection-aware**: `state.focus` means "the focused cursor row"
(the same cyan as the `▸`/title, §4.1), so the bar earns it ONLY when the row is
`selected and list_focused`, and there it OVERRIDES the per-status color, so the
cursor always owns the single brightest bar (a selected completed row must out-rank
an unselected watching one). Off that row the bar drops to the status color, and an
unselected watching bar can never impersonate the cursor. The two cursor rows below
override ALL statuses; the canonical rules are `render::bar_fill_color` /
`render::bar_frac_color` (`src/tui/render.rs`, both unit-tested).

| State | Condition | Bar fill color | Fraction color |
|---|---|---|---|
| **Cursor, list focused** (any status) | `selected and list_focused` | `state.focus` (`dim` if paused) | `text.muted` for watching/paused, else `text.dim` |
| **Cursor, detail focused** (any status) | `selected and !list_focused` | `text.muted` (`dim` if paused) | `text.dim` |
| Watching, unselected | `!selected` | `text.muted` | `text.dim` |
| Paused, unselected | `!selected` | `text.muted` dim | `text.dim` |
| Completed, unselected | `!selected` | `text.dim` | `text.dim` |
| Dropped, unselected | `!selected` | `text.dim` | `text.dim` |
| Planning, unselected | `!selected` | `border.hair` (empty bar) | `text.dim` |

### 4.6 Episode Grid Cell

The episode grid is rendered in the detail pane below the metadata, as a grid of
numbered cells. Cell width: 5 chars (`[NN] ` with trailing space for 2-digit
episodes, `[NNN]` without trailing space for 3-digit). Cells wrap to fill the
available column width.

| State | Glyph | Background | Foreground |
|---|---|---|---|
| Unwatched | `[NN]` | `bg.base` | `text.muted` |
| Watched | `[NN]` | `bg.base` | `text.dim` + dim |
| Currently watching (resume) | `[NN]` | `bg.surface` | `state.focus` + bold |
| Resume point | `[▸N]` | `bg.surface` | `state.now` + bold |
| Focused (cursor on grid) | `[NN]` | `bg.surface` | `state.focus` + bold |
| Launching (resolving / playing) | `[⠋]` | `bg.surface` | `state.focus` + bold → `state.now` + bold at >3s |
| Airing/not-yet-released | `[NN]` | `bg.base` | `text.dim` + italic |

The resume point cell (`[▸N]`) is always the most visually prominent cell in the
grid; `state.now` is only ever earned by one cell at a time. A resume point is
the freshest partial watch recorded **since the frontier last rose**, whichever
writer raised it (local ratchet, recompute, AniList sync); a partial the frontier
has passed is dead, a partial written after it is a live rewatch (ROD-477). Only
a rise retires a partial: a sync correction that lowers progress leaves the
frontier behind the partial, which keeps it live (ROD-497).

**Launching cell state.** When playback is resolving (the 2-3s resolve → mpv-launch
window), the played episode's cell renders the current braille spinner frame
(`spinner_char()`) in place of its number, inside the same `[ ]` shell so it reads
as *that cell* working rather than a free-floating glyph. Background and bold match
the focused state; colour follows the `is_slow_path()` rule: `state.focus` for the
first 3s, `state.now` beyond, identical to the bottom-bar and cover-block spinners
(§4.8). This is the **primary** in-progress affordance for playback: it sits at the
user's attention locus (the cell they just pressed Enter on), not the bottom-left
corner. It tracks the *session*, not the cursor; the grid stays navigable during
play (mpv is a separate window), so the spinner stays pinned to the playing episode
on its own show. It outranks the focus and watched states. On a completed watch it
resolves directly to watched (no intermediate frame) as the cursor advances; on a
partial or failed play it returns to focus and the cursor holds (§4.10).

**Grid region states (no cells to draw).** Before any cell renders, the grid region
resolves one of three non-cell states, which must read as distinct:

| State | Render | Voice |
|---|---|---|
| Fetching | `⠋ loading episodes…` in `state.focus`, top of region | active, spinner |
| Genuinely zero episodes (`episodes_done`, empty array) | `no episodes` in `text.dim` + italic, **centered** | deliberate absent state |
| Resolve walk exhausted, nothing landed | `no source` in `text.dim` + italic, **centered** | terminal absence (the walk's failure classes already toasted; ROD-439) |
| No fetch fired (no item selected) | nothing | blank by design |

The zero-episode case is a real provider result, *not* a failure; a fetch error
toasts instead (`episodes_error`, §4.10) and never reaches the grid. It is centered
+ dim (`text.dim`) to match the non-actionable absent states (`no art yet` is also
`text.dim`), while the actionable first-run CTAs (`search the catalogue`, `nothing
watched yet`) sit one tier brighter at `text.muted` (§8.3, §10). It reads as
"nothing here," not a half-drawn loading row pinned to the top-left.

### 4.7 Toast Notifications

Toasts float above the bottom bar, right-aligned, temporary (2.5s auto-dismiss).
Single line. Max width: 40 display columns, the whole box, glyph prefix included.
The `[!] `/`[✓] `/`[~] ` prefix is a fixed 4 columns, so the **copy budget is 36
columns**. The single source of truth lives in code as `Toast::MAX_BOX_COLS` /
`GLYPH_COLS` / `MAX_COPY_COLS`. Dynamic copy that would exceed it (only
`task_error`'s error-name payload today) is truncated on a grapheme boundary with a
trailing `…`; static copy is all well under.

A `persistent: bool` field on `Toast` marks the source-unreachable variant, which
does not auto-dismiss; it clears on recovery (§8.5). The auto-dismiss rule is the
default, not the only mode.

Format: `[!] something failed · details`

| Type | Left glyph | Background | Foreground |
|---|---|---|---|
| Info | `[~]` | `bg.elevated` | `text.muted` |
| Success | `[✓]` | `bg.elevated` | `state.success` + bold |
| Error | `[!]` | `bg.elevated` | `state.now` + bold |
| Warning | `[!]` | `bg.elevated` | `state.warn` |

Toasts appear at row `terminal_height - 2` (one row above the bottom bar). No
animation; they appear and disappear on the cell grid with no transition. If
multiple toasts queue, they stack upward (row -3, -4, etc.), max 3 visible.

See §4.10 for the canonical event→feedback mapping: which actions earn a toast,
which kind, persistent vs transient, and which are deliberately silent.

### 4.8 Loading / Spinner

Used when: cover art is fetching, search results are loading, AniList sync is in
progress, or playback is resolving (mpv launch in flight, surfaced as the §4.6
launching cell, with the bottom bar as a secondary signal). See §4.10 for the
in-progress vs. terminal-outcome decision rule.

Spinner frame sequence (cycles at ~100ms per frame):
```
⠋  ⠙  ⠹  ⠸  ⠼  ⠴  ⠦  ⠧  ⠇  ⠏
```
(Braille spinner: clean, small, universally supported.)

Color: `state.focus` when fetching normally. `state.now` when something is slow
(>3s, a design-level definition of "slow").

In the cover art block: spinner rendered centered in the cover cell region, on
`bg.surface` fill.

In the bottom bar: `[~]` prefixes the status text during a sync.

### 4.9 The Magenta Cursor

The `▌` lives at the leftmost position of the bottom bar. It is steady, always
`state.now`. The original ~1hz blink is retired (ROD-481): ghostty never rendered
SGR blink, kitty does, and a blinking block in the corner reads as a stray
terminal cursor, not a status marker.

It is suppressed (replaced by the prompt character) when the command line is active
in search or command state, and by the `[!]` glyph in the confirm state.

Nothing blinks in this UI. If something seems like it should blink, it should not.
Use color weight change instead.

### 4.10 Toast Event Matrix

**Design rule:** in-progress state = §4.8 spinner; terminal outcome (done or
failed) = §4.7 toast. These two channels are not interchangeable. A spinner
mid-operation is not a promise of a toast when it resolves; only outcomes the user
must be aware of earn a toast. Deliberate silences are documented here; unlisted
events are silent by design. The spinner must also land at the user's attention
locus; see the §4.6 launching cell for why playback resolves *in the grid*, not
only the bottom-left corner.

**Exception:** `play_retry` breaks this rule on purpose; it toasts mid-operation,
not on a terminal outcome. A stream-open failure triggers a re-resolve + relaunch
after a 2-4s backoff, and a silent multi-second wait reads as a frozen launch; the
toast makes the backoff legible without waiting for the eventual
`play_error`/`play_done`. See its row below.

**In-progress (spinner, §4.8), bottom-bar spinner active while in flight:**

| Async operation | State flag | Primary locus |
|---|---|---|
| Search (debounce + AniList fetch) | `search_loading` / `debounce_deadline_ms` | bottom bar |
| History load (startup DB read) | `history_loading` | bottom bar |
| Episode grid fetch (provider) | `episode_loading` | bottom bar |
| Cover art fetch + decode | `cover.loading` | cover block + bottom bar |
| Playback resolving (resolve → mpv launch) | `playing` | **episode cell (§4.6)**; bottom bar secondary |

All five share `async_start_ms` + `is_slow_path()` for the >3s `state.focus →
state.now` escalation.

**Terminal outcome (toast, §4.7), fires on a resolving event:**

| Event | Condition | Kind | Copy | Persistent |
|---|---|---|---|---|
| `play_done` / `play_error` | completed watch (final position ≥ `NATURAL_END_RATIO`), not finale | success | `episode N done` | no |
| `play_done` / `play_error` | completed watch, finale | success | `all caught up` | no |
| `play_error` | mpv not on PATH / not installed (`MpvNotFound`) | error | `mpv not found · install mpv` | no |
| `play_error` | mpv launched but exited non-zero (`MpvFailed`) | error | `mpv exited with error` | no |
| `play_error` | no observed position; non-HTTP, non-mpv failure | error | `playback failed` | no |
| `play_error` | resolve failed: network-down (timeout / refused) | error | `network unreachable` | no |
| `play_error` | resolve failed: blocked (403 / 451) | error | `{provider} blocked us` | no |
| `play_error` | resolve failed: server-down (5xx) | error | `{provider} is down` | no |
| `play_error` | resolve failed: other non-200 | error | `{provider} returned an error` | no |
| `play_retry` | mpv open failed, retry budget remains | warn | `stream didn't open · retrying N/M` | no |
| `play_error` | mpv open failed, retry budget exhausted (`MpvOpenFailed`) | error | `stream didn't open · try again` | no |
| `episodes_error` | network-down (timeout / refused) | error | `network unreachable` | no |
| `episodes_error` | blocked (403 / 451) | error | `{provider} blocked us` | no |
| `episodes_error` | server-down (5xx) | error | `{provider} is down` | no |
| `episodes_error` | other non-200 | error | `{provider} returned an error` | no |
| `episodes_error` | data-shape failure (no episode data / OOM) | error | `couldn't load episodes` | no |
| `task_error` | background task failed | error | (payload) | yes |
| Search source unreachable | non-200 / network fail on the AniList search call | error | `can't reach AniList` | yes |
| Settings saved | write succeeded | success | `settings saved` | no |
| Settings: no config dir | dir missing, skipped | warn | `no config dir · not saved` | no |
| Settings save failed | write error | error | `settings save failed` | no |
| `progress_reset` | selected show present (r key) | success | `progress reset` | no |
| `undo` | undo of a status mutation (u key) | info | `undone` | no |
| `add_to_watchlist` | P on a browse result (upsert ok) | success | `added to watchlist` | no |
| `add_to_watchlist` | P on a browse result (upsert failed) | error | `couldn't add to watchlist` | no |
| `sync_flushed` | pull reconciled remote changes (`reconciled > 0`) | info | `↓ N from AniList` | no |
| `sync_flushed` | push landed (`pushed > 0`) | info | `↑ N to AniList` | no |
| `update_available` | boot check found a strictly newer release (06 §6.1) | info | `update available: vX.Y.Z` | no |
| Provider fallback hop | the resolve walk moves to the next registry provider | warn | `trying {provider}…` (or `{prev} failed, trying {provider}…`) | no |
| Provider pin: hop | pinning a different provider than the one serving the grid re-routes it through a one-provider fallback walk (reuses the hop toast) | warn | `trying {provider}…` (or `{prev} failed, trying {provider}…`) | no |
| Provider pin: set, no hop needed | `v` pins the provider already serving the grid | success | `pinned to {provider}` | no |
| Provider pin: cleared | `v` cycles past the last provider back to unpinned | info | `provider pin cleared` | no |
| Provider pin: hop failed | the pin's one-provider walk could not even run (worker spawn failure; freeze: `advanceFallback` returned false). Distinct from a walk that ran and missed | warn | `couldn't reach {provider}` | no |
| Provider pin: flip missed | the pin's one-provider walk ran (probe/search) and found no match; the pin is kept (03 §5.1, ROD-439) | warn | `no match on {provider}, pin kept` | no |
| Provider pin: nothing to pin | `v` pressed with no focused episode source | info | `no source: nothing to pin` | no |
| Provider pin: row not minted yet | `v` pressed before the serving provider's binding row is minted (only happens on `episodes_done`) | info | `still resolving, try again shortly` | no |
| Provider pin: store write failed | the pin write errors on set or clear | error | `couldn't save the provider pin` / `couldn't clear the provider pin` | no |
| Resolve walk exhausted | every provider tried or skipped, no grid landed (§4.6 `no source` state) | error | `no source found` | no |
| Forced-preferred miss | the §5.3 stale-stamp probe missed; the K-2 continuation walk begins | warn | `no match on {provider}` (distinct from the pin-kept copy by law) | no |
| Play continuation: remap miss | a play-fallback hop landed a sibling grid without the in-progress episode (exact raw label, else 1-based ordinal); play continuation stops, the walk's toasts already ran (03 §6.4/§7, ROD-439) | error | `episode {raw} not found on {provider}` (`{raw}` is provider text, control-stripped) | no |
| Delete refused: playing | `y` on an armed delete while that show is the live playback (ROD-220); the confirm disarms, nothing is deleted | warn | `can't delete, currently playing` (freeze copy) | no |

Copy: single line, lowercase, no terminal punctuation; status, not prose, and
within the §4.7 36-column copy budget (the box is 40 cols incl. the 4-col glyph
prefix). The one dynamic `(payload)` above, `task_error`, is truncated to fit with
a `…`. **Persistence** is reserved for *ongoing* conditions still true while the
toast is visible (source unreachable). Point-in-time failures (play, episodes) are
transient; the condition is already over and the user can retry.

`{provider}` above is the acting provider's display name,
`StreamProvider::display_name()`. The registry holds multiple concrete providers,
so no copy above the provider trait hardcodes a site name: the same string
formats for whichever provider the walk landed on. `display_name()` is distinct
from `name()`, the stable persistence key. The name is formatted in at runtime; a
short name keeps these within the 36-column budget, and a long-named provider is
truncated by the toast push. `network unreachable` carries no `{provider}`; it
names the user's own connectivity, not a provider. `can't reach AniList` names
AniList directly: the catalog brain is a single fixed dependency and sits outside
the provider registry. The provider row's tokens show `name()`, not
`display_name()` (§5.3a; §10 logs the split).

The four provider cause classes (`network-down`, `blocked`, `server-down`,
`generic-http`) share copy between `play_error` (resolve path) and
`episodes_error`; cause determines the string, context is inferrable from the
user's last action. `play_error` adds two **player-spawn** classes: `MpvNotFound`
and `MpvFailed` get their own copy, the install-directive one earning the
actionability the generic line couldn't. `playback failed` means only a residual
non-HTTP, non-mpv failure. The runtime source of truth for **all the `play_error`
/ `episodes_error` class rows** is one shared failure-class → copy mapping
(`failure_class_copy` in `src/tui/app.rs`); those rows and that mapping move
together.

A watch counts as *watched* (bumps the progress high-water mark, dims the cell,
advances the cursor) only when the final position reaches `NATURAL_END_RATIO`
(0.80) of the runtime; a clean mpv quit is not proof of a watch (you can quit at
any second). This is the same bar the store uses for resume "done," so the progress
count, the §4.6 dim, and the cursor advance never disagree. A *partial* watch is
still a real play (it lands in history with a resume point) but does not advance N.
Accordingly a completed `play_error` (errored at the very end) takes the success
path; any non-completed `play_error` fires `playback failed`. The two are mutually
exclusive in the playback-finish path.

The two `sync_flushed` rows are the git-style ahead/behind idiom: `↓ N from
AniList` when a pull reconciled remote changes into local rows (the launch pull or
an action flush's pull half) and `↑ N to AniList` when the action flush pushed
local changes up, both ambient background-sync confirmations rather than direct
user-triggered outcomes. They ride the one shared `sync_flushed` event and are
independent; a flush that moved both directions enqueues both, in execution order
(reconcile, then push). A reconciled remote change re-baselines the sync snapshot,
so it counts once and never re-toasts on later flushes. A no-op sync and every soft
failure (rate-limit, transient transport, rows left dirty for the next retry) stay
silent.

**Deliberate silences** (no toast, no spinner; documented intent, not oversight):

| Event | Why silent |
|---|---|
| `search_done` | The result count in the list is the feedback; a count toast mid-type is noise. |
| `episodes_done` | The grid appearing in the detail pane is the feedback. |
| `history_loaded` | The watchlist populating on startup is the feedback. |
| `cover_done` | Image appears in-pane. |
| `cover_error` | Cover is supplementary; the "no art yet" absent state (§8.1) handles the gap, no user action needed. |
| `play_done` (uncounted) | mpv exited clean with nothing observed: a cancel. No advance, no feedback. |
| `position_update` | Live telemetry. |
| `focus_in` / `focus_out` / `winsize` | Terminal lifecycle; layout reflows silently. |
| `tick` | Internal heartbeat. |

---

## 5. Annotated ASCII Mocks

Color annotations use token shorthand: `[fg]` = `text.primary`, `[m]` = `text.muted`,
`[d]` = `text.dim`, `[f]` = `state.focus`, `[h]` = `state.now` (hot/magenta).

### 5.1 Browse: Idle

Terminal width: 120 cols. List col: 44 cols. Detail col: 74 cols.

```
                                                                                         [context: top bar, full width]
  SABIGOKU  ░  Browse  冬 2026                                                    ·      [h1+bold fg] [d] [f] right: [f]·
                                                                                         [spacer row]
  ▸ Frieren: Beyond Journey's End        ✦ [96/100]  [   COVER ART IMAGE         ]     [focused row: bg.surface, f+bold title, h score+bold]
  · Fullmetal Alchemist: Brotherhood       [97/100]  [   kitty graphics          ]     [default row: fg title, h score]
  ◉ Vinland Saga                           [92/100]  [   or half-block fallback  ]     [airing row: h◉, fg title, fg score]
  ● Mob Psycho 100                         [91/100]  [                           ]     [watched row: d● d title d score]
  · Steins;Gate                            [89/100]  [                           ]     [default]
  · Attack on Titan                        [87/100]  [                           ]     [default]
  · Neon Genesis Evangelion                [84/100]  Frieren: Beyond Journey's End      [fg+bold, wraps to detail col]
  · Made in Abyss                          [83/100]   放映中  冬 2024                   [h chip, m chip]
  · Demon Slayer                           [81/100]  ✦ [96/100] · Fantasy · Adventure  [h+bold score, d·, m genres]
  · Jujutsu Kaisen                         [80/100]  ─────────────────────────────     [border.hair rule]
  · Chainsaw Man                           [78/100]   28 eps  · TV                     [m metadata]
  · Spy × Family                           [76/100]  ─────────────────────────────     [border.hair rule]
                                                       An elf mage who once defeated…   [m synopsis, word-wrapped]
                                                       the Demon King now wanders the
                                                       continent without purpose, until
                                                       she meets a young girl…
                                                                                         [spacer]
  ▌  hjkl · / search · : command · q quit                                               [h▌ steady, d text, m+bold keys]
```

> **Score tokens in the Browse wireframes (§5.1, §5.2) are drawn in the long
> `[NN/100]` form for column legibility.** List rows render the compact `[NN]`
> badge (no `/100`, no `✦`; both detail-pane only) per §2.2/§4.3, right-anchored
> against the list pane edge, with the episode count seated to its left on a wide
> pane (title > score > eps). The grids are not re-rendered to the compact form;
> this note is the reconciliation.

### 5.2 Browse: Search Active

The user pressed `/`. The bottom bar becomes the search prompt. The list filters live.

```
  SABIGOKU  ░  Browse  冬 2026                                                    ·

  ▸ Frieren: Beyond Journey's End        ✦ [96/100]  [   COVER ART IMAGE         ]
  · Fullmetal Alchemist: Brotherhood       [97/100]  [                           ]     [results filtered to query]
  · FMA: Brotherhood (2009)                [97/100]  [                           ]
  · Free! (Swimming)                       [74/100]  Frieren: Beyond Journey's End
  · From the New World                     [71/100]   放映中  冬 2024
  · Fruits Basket                          [70/100]  ✦ [96/100] · Fantasy · Adventure
                                                     ─────────────────────────────
                                                      28 eps  · TV
                                                     ─────────────────────────────
                                                      An elf mage who once defeated…




  /  fr_                                                            [catalogue · 6]    [f+bold /, fg+bold input, m count]
```

Notes:
- The list filtered from 12 to 6 results immediately on keystroke.
- The `▌` is gone; the `/` takes its visual position, static, `state.focus`.
- The `_` character after `fr` is the text cursor: `state.focus`.
- Result count is right-aligned in `text.muted`, with the `catalogue` scope tag
  (§8.4).

### 5.3 Detail Zoom: Full-Screen

The user pressed `Space` from a focused detail pane (`active_pane = .detail`);
promotion has no width gate, any two-pane width qualifies (`w ≥ PANE_SPLIT_MIN`);
the 120-col terminal below is one illustration of it. `active_view` becomes
`.detail` (full-screen zoom). The list is gone; the canvas is all detail. `Esc`
demotes back to the two-pane view with `active_pane = .detail`. This surface is
reached identically from Browse, History, and Discover.

120-col terminal, zoom entered from History. The metadata is the compact `28 eps ·
TV` line at every origin and width (§5.3a); the provider row rides the top of the
episode grid, not the show info:

```
                                                                                         [context: top bar, full width]
  SABIGOKU  ░  Watchlist  冬 2026                                                 ·      [h1+bold fg] [d] [f] right: [f]·
                                                                                         [spacer row]
  [   COVER ART IMAGE   ]   Frieren: Beyond Journey's End                               [left col: cover block; right col: title fg+bold]
  [   20 × 7 cells      ]    放映中  冬 2024                                            [h chip, m chip]
  [                     ]   ✦ [96/100] · Fantasy · Adventure · Drama                   [h+bold score, d·, m genres]
                            ─────────────────────────────────────────────────────       [border.hair]
                            28 eps · TV · Manga · 24 min · Madhouse · #12 rated 2024     [compact meta line, m; Rank last, sheds first]
                            ─────────────────────────────────────────────────────       [border.hair]
                             An elf mage who once defeated the Demon King now            [m synopsis, word-wrapped]
                             wanders the continent without purpose, until she
                             meets a young girl named Fern…
                            ▸megaplay ?senshi ?allanime · [v]                            [provider row atop the grid; serving fg, rest m, [v] cycle hint]
                            [1][2][3][4][5][6][▸7][8][9][10][11][12]               [d watched, h▸ resume, m unwatched]
                            [13][14][15][16][17][18][19][20][21][22][23][24]
                            [25][26][27][28]

  ▌  hjkl scroll · enter play · v provider · space/esc back                                  [h▌, d help, m+bold keys]
```

Notes:
- Full canvas width. The list is gone; this is the zoom surface.
- `active_view = .detail`. `detail_origin` records the origin (`.browse`,
  `.history`, or `.discover`). `Esc` or `Space` demotes back to the two-pane,
  `active_pane = .detail`. `q` quits the app (§7.4).
- `[▸7]` is the resume cell: `state.now` + bold, prefixed with a `▸` glyph, the
  most visually prominent cell in the grid (§4.6). The arrow is the **only** glyph
  in the grid (the most actionable cell earns the loudest mark). Resume reads apart
  from the focus cursor by **hue**: resume is `state.now`, the cursor is
  `state.focus` + the `bg.surface` band that stays the cursor's alone. (For 3-digit
  / non-numeric labels the `▸` drops, no room in the 5-wide cell, and the
  `state.now` colour carries resume on its own.)
- Watched cells (`1`–`6` here) recede via `text.dim`, **no glyph**. A filled mark
  like `●` would out-weigh the resume arrow and invert the hierarchy (the done,
  receding cells shouting louder than the one you should act on), so watched is
  conveyed by colour alone.
- Unwatched cells (`8` onward) are `text.muted`.
- Cover art uses the full left column width for the tier calculation (§3.3). At
  120 cols, `left_w ≈ 44` → 20-col cover applies.
- `Space` is a zoom toggle: it promotes from detail pane and demotes from zoom.
  `Esc` also demotes (and is the "canonical back" key throughout the app). Both are
  shown in the help line as `space/esc back`.
- The two-column internal split (cover left / content right) uses the same
  `left_w = max(20, pane_w * 38 / 100)` formula. At ≥ 160-col the layout gains
  density (§5.4a) with `right_w ≈ 96` giving ≈ 19 grid columns.
- The metadata is the compact `· `-joined line (§5.3a): Episodes, Format, Source,
  Duration, Studios, Rank. Provider and pin are not on it; they ride the top of
  the episode grid as the `▸{serving} … · [v]` row. The `nextAiringEpisode`
  countdown lands on the chips row (§4.4), not here. `v
  provider` in the help line cycles the provider pin; it is live on this zoom
  surface, same as the in-pane grid.

### 5.3a Detail Metadata: The Compact Line

One ordered field list, one density at every width and origin. `detail_meta_fields()`
(`src/tui/view/detail.rs`) returns an ordered list of `MetaField` (`{value, unit,
dim}`), highest-priority field first: **Episodes**, **Format**, **Source**,
**Duration**, **Studios**, **Rank**. All six are AniList metadata about the show.

These six render on the **compact line** (`meta_line`), values joined
with ` · ` on one row (separator `fg3`, values `fg2`, `fg3` when a value's `dim`
flag is set). A unit suffix renders here (`13 eps`; Format carries no unit), and a
separator sits only *between* two emitted fields, so an absent field never leaves
an orphan `·` (§8.1). **Rank rides last**, so it is the first field the line sheds
when width tightens.

**Provider and the in-flight probe ride the grid, not the show info,** and are not
fields at all: `provider_line` derives them straight from the live session,
reading the provider bindings (`anime` binding rows + `provider_absences`) and
the serving provider off the session, plus whichever token an active walk is
currently probing (ROD-525; nothing here is read from a stored per-show
preference table). They render as a dedicated row at the top of the episode
grid, `{tokens} · [v]`, where `[v]` is the cycle-provider affordance (the `v`
key, §7.5 keybind-hint styling). The row appears only when the grid is engaged
(a focused detail surface), because provider/walk state is a "how this will
play" affordance that belongs with the grid you play from, not the show
metadata. (This replaces the earlier compact-line-vs-labeled-rail design; the
§5.3a rail and its `bloom`/`two_col` gating are gone. ROD-458.)

**Serving and the in-flight probe are encoded on the fg ladder, not as text**
(ROD-484; ROD-525 retires the pin register in favor of this). Each token is
`{marker}{name}` in fixed registry order; the boosts are per token and
independent:

| Token | Treatment |
|---|---|
| Non-serving, not being probed | `fg2` |
| Serving | `fg` |
| Aimed (`v` settle window armed) or being probed (mid-walk, auto or manual) | the selection cursor: elevated register; exact glyph/weight settles in design review, §10 |

`fg3` stays reserved for the nothing-known row (every provider unchecked), which
dims whole; a healthy non-serving token never drops to it.

- **The bold pin register is replaced by a probing token** (ROD-525). There is no
  longer a persistent per-show preference for the UI to mark: last-used (03 §5.1)
  is an internal default the walk starts from, not a user-facing choice, so an
  ordinary open has nothing to render beyond `serving`. The elevated register
  instead marks the **selection cursor**: the aim while the `v` settle window
  is armed (every press must visibly move it before any walk fires, or the
  burst reads as dead input), then whichever token the in-flight walk hop is
  currently probing, auto-fallback or manual `v` alike. A walk is exactly the window where `serving`
  (`▸`) is stale, since the hop hasn't landed yet, so the probing token is the
  only truthful selection signal until it does. It clears the instant the hop
  lands, at which point `▸` on the landed token carries the truth again. This
  retires ROD-484's confirmed-marker gate and ROD-524's ungating of it along with
  the pin itself; both governed a register that no longer exists (§10 logs the
  lineage).
- **Silent provider migration is accepted, and it is sharper than it sounds:**
  once a show lands on a sibling provider, nothing re-probes the original
  (prewarm only probes *unchecked* providers, and a bound original is never
  re-probed), so the original is reachable again only by another manual `v`.
  This is a direct cost of retiring the pin: the pin at least named an intended
  provider in the DB even while unrendered; last-used names only wherever the
  walk last landed.
- **Keybind-hint bold and state bold are different registers** and may coexist in
  one row: the `[v]` hint is bold per §7.5 while a probed token is bold per this
  section, live only during an active walk, which is the carve-out to §1.3's
  "use it once per visual unit". Bold alone is the ratified differentiator here
  and stays phosphor-safe (§1.4).

**Episodes is the floor.** It always renders, never omitted: when `total_episodes`
is unknown (no show focused, or a show with no metadata yet) it degrades to a dim
`?` (`fg3`, the `dim` flag) instead of disappearing, so the line is never empty.
Every other field can be omitted outright: each simply isn't emitted when its
underlying value is null (§8.1: no orphan separator).

**Field formatting:**

- **Source**: AniList `source` enum (`MANGA`, `LIGHT_NOVEL`, `ORIGINAL`,
  `VISUAL_NOVEL`, `GAME`, `WEB_NOVEL`, …), rendered title-case with underscores
  turned to spaces: `LIGHT_NOVEL` → `Light novel`, `ORIGINAL` → `Original`.
  ~~Rail label `Source`.~~ Nullable column, mirrors `kind`.
- **Duration**: AniList `duration` (per-episode runtime, minutes), rendered
  `{n} min` (e.g. `24 min`); omitted when null or zero: a 0-minute runtime is a
  missing value, not a fact. ~~Rail label `Duration`.~~ Nullable column.
- **Studios**: AniList `studios{nodes{name}}`, narrowed to *main* animation
  studios via AniList's `isMain` flag (`studios(isMain:true){nodes{name}}`).
  Persisted as its own `studios` column ('\n'-joined blob, split on read,
  `COALESCE` on upsert so a null re-fetch never clobbers a stored list; the §8
  DB-safety rule applies to every nullable metadata column, not just this one).
  Collapse-format: `A` for one studio, `A, B` for two, `A, B +N` beyond two,
  capped at 2 named studios ~~so a long co-production credit list can't blow out the
  rail's gutter~~ (§10 logs the cap). ~~Rail label `Studios`.~~
- **Rank**: AniList `rankings{rank type context year season allTime}`. Selection
  prefers a **contextual** ranking (`allTime: false`, season- or year-scoped) over
  an all-time one when both exist; render `#{rank} rated {year}` / `#{rank}
  popular {year}` for a contextual hit, or `#{rank} rated` / `#{rank} popular` for
  the all-time fallback. The season name is dropped even for a season-scoped
  ranking, since the header's own season/year chip (§4.4) already carries that
  context on the same screen. When both a contextual RATED and a contextual
  POPULAR ranking exist, RATED wins the tie-break (§10). ~~Rail label `Rank`.~~
  Persisted as three pre-selected scalar columns, `rank` / `rank_type` /
  `rank_year`, rather than a raw blob: `select_rank` picks the best ranking once
  at fetch time, so render just composes the stored values.
- **Provider**: ~~rail label `Provider` (8 chars, fills the gutter with no
  truncation).~~ It folds two related but distinct signals into one value string,
  one token per registry provider in fixed **registry (construction) order**, not
  the per-walk preference order (a resolve-order hint, not a stable reference
  list; reordering this line per session preference would make the same show's
  ~~rail~~ line read differently across sessions, fighting the "scan the same
  column, same order, every show" habit the ~~rail~~ line is built for):

  - **availability**: does a binding exist for this provider (the canonical row
    joined on that source), does a fresh negative exist (`provider_absences`,
    7-day TTL), or is neither known (never probed, or the negative went stale)
  - **serving**: which provider the *currently loaded* episode grid was actually
    fetched from (`episodes.for_source`, the fetch identity plays fire on),
    routing truth, distinct from what an in-flight walk is probing toward

  Each provider renders as one marker glyph plus its raw name (raw, not
  `display_name()`): `▸name` if that provider is
  serving the open grid (implies bound, since a fetch can't succeed against an
  unbound provider), `+name` if bound but not the one serving right now, `-name`
  if a fresh negative exists, `?name` if unchecked. At most one `▸` ever appears
  in the line. Tokens are space-joined in registry order, e.g. `▸allanime
  +altsrc` (allanime serving, altsrc also bound elsewhere) or `▸allanime ?altsrc`
  (altsrc never probed). `▸` is the existing resume/active glyph (§4.6), already
  proven legible dim; `+`/`-`/`?` are deliberately plain ASCII rather than a new
  pictographic set (chosen while the diamond/circle pairs' dim legibility was
  still open, §11.4; the ASCII set stands on font-independence regardless).
  Shape, not color, carries the state: the Phosphor theme is monochrome (§1.4,
  `focus`/`fg` share one hue), so any UI state that only
  color could tell apart is illegible there by construction, and the four markers
  are distinct shapes for exactly this reason. The whole value dims to `fg3` only
  when every provider is `?` (nothing known about any of them, mirroring the
  Episodes-floor "we know nothing yet" degrade); it renders `fg2` as soon as at
  least one provider is bound or confirmed absent, since a fresh negative is real
  information, not a gap.

  Gated on canonical identity only, the same floor Probing below uses:
  omitted outright when the show has no AniList id (no `provider_last_used` /
  `provider_absences` FK target either), emitted otherwise even if every provider
  comes back `?` (a genuinely fresh, unresolved identity). A grid loaded from a
  provider no longer present in the registry (a retired source) degrades
  silently: no registry slot to hang a `▸` on, so the line renders the live
  providers' availability with no serving marker at all until a later resolve
  lands on a still-registered one. Not an error, not a crash, just a plain
  "nothing current is confirmed serving." Nav-state only, same rule as Probing
  below and for the same reason: `detail_meta_fields_for` (fed an explicit
  record, e.g. the History list preview) never appends it, because the serving
  half needs the live session's `episodes.for_source`, which belongs to the
  currently *focused* grid, not a cursor row a preview might be scrolled past.
  Keeping that exclusion on the whole field, not just the serving half, keeps the
  rule one clean boundary ("session-derived fields live in the wrapper") rather
  than splitting Provider's two halves across two availability rules. The design
  targets a registry of 2–4 providers; a longer registry needs a fresh look at
  this line's width budget.
- **Probing** (ROD-525, replaces **Pin**): not a field, and not a stored value
  even under the retired design; it is read straight off the in-flight walk
  state for the session's focused grid (`EpisodeSession::walk`), the same
  live-session-only rule the Provider note above states for `serving`. There is
  nothing to persist here and nothing to migrate: `provider_pins` is dropped
  (02 §3.2/§3.3) without leaving a UI-facing successor table, because the
  elevated register now marks a transient walk position, set via the `v` key on
  any detail surface (§7.5), not a stored per-show preference. Absent whenever no
  walk is in flight: presence or absence, not a §8.1 degrade. Neither Serving nor
  Probing is a field, so neither carries a shed rank; the AniList sextet is the
  whole `detail_meta_fields` list.

~~**Compact form: Provider and Pinned get their own dedicated row.** Below
`DETAIL_TWO_COL_MIN` the rail never blooms, so a `rail_only` Provider and Pinned
would be invisible on every compact-width detail pane, exactly the width most
terminals run at day to day. They are not folded into the `·`-joined meta line:
routing/session state is a different category of fact from the AniList metadata
that line carries, and interleaving muddies both (§10). Instead, in the
non-bloomed form only, `draw_header` draws one additional row directly beneath
`draw_meta_line`'s output, before the synopsis hairline: `draw_provider_line`,
which is not part of the generic field-list iteration either renderer uses. It
finds the already-computed Provider and Pinned entries in the same field list (a
small linear scan by label, at most eight entries) and composes them into one
bespoke row, reusing their `value`/`dim` as already computed for the rail; no
recomputation, no new app-level state.~~

~~Grammar: Provider's segment reuses its rail value verbatim (`▸allanime +altsrc`);
the marker glyphs already self-describe. When a pin exists, a `·` separator
(`fg3`, matching the meta line's own separator) plus a `pin ` prefix plus the
pin's raw provider name follow: `▸allanime +altsrc · pin altsrc`. The `pin `
prefix is required: on this row the pin sits directly next to Provider's own token
list, and without the prefix a trailing bare provider name reads as an unmarked
third provider token rather than the pin. When unpinned, the row is just the
Provider segment, no trailing separator. Pinned's segment is always `fg2` when
shown (no dim state; presence or absence). The `pin ` marker is a literal composed
directly in `draw_provider_line`, not a `MetaField`-level mechanism; `MetaField`
stays exactly `{label, value, unit, dim, rail_only}` (§10).~~

~~Omission: the whole row is skipped, no row consumed, when the show has no
canonical identity, the same gate Provider itself uses. Placement is fixed
directly under the meta line and above the hairline into synopsis. A
height-starved pane drops this row the same way every other single conditional
row in `draw_header` does; no new shed mechanism, since this row sits outside the
rail's own height-shed loop entirely.~~

(This whole "compact form" special case is gone: `provider_line` is now the only
form, unconditional, drawn atop the episode grid rather than beneath the meta
line. Current grammar and omission rule are stated above. ROD-458.)

The `nextAiringEpisode` countdown does **not** join this field list; it renders on
the **chips row** (`state.now`, §4.4) instead, because it is a live,
clock-relative signal, not a stored snapshot ~~the rail's~~ (the compact line's,
ROD-458) static model fits. ~~Both renderers iterate~~ `meta_line` iterates the
field list generically ~~(plus the one shared `rail_only` skip in
`draw_meta_line`)~~ with no per-field special cases left (ROD-484), so a new
field is a `detail_meta_fields` data change only, no further renderer edits. The full survey of AniList fields considered and
rejected for this list is in §10 (metadata field survey).

### 5.4 History / Watchlist: Narrow (w < 60) or No Records

Below 60 cols, or when no records exist (§8.3 empty state), History renders as a
single-column full-width list. No detail pane. The mock below also doubles as the
canonical list-side content reference; the same rows appear in the left pane of
the wide two-pane layout (§5.4a).

**Narrow / empty: single column (w < 60, or no records):**

```
  SABIGOKU  ░  Watchlist  冬 2026                                                 ·

  ▸ watching (4)
  ─────────────────────────────────────────────────────────────────────────────────
    ▸ Frieren: Beyond Journey's End                         [▸12] 冬 2024  放映中
      [████████◐░░░░░░░]  6 / 28 eps  · resume ep 7 · last watched 3 days ago
                                                                                     [f bar, f◐ at ep6, m metadata]
    ▸ Vinland Saga S2                                      [  1] 冬 2023  完結
      [░░░░░░░░░░░░░░░░]  0 / 24 eps  · not started
                                                                                     [border.hair bar (planning), m meta]
    ◐ Blue Period                                          [◐ 5] 秋 2021  完結
      [██████◐░░░░░░░░░]  5 / 12 eps  · paused · last watched 2 weeks ago
                                                                                     [f dim bar, m meta]
  ─────────────────────────────────────────────────────────────────────────────────

  ▸ completed (12)
  ─────────────────────────────────────────────────────────────────────────────────
    ● Fullmetal Alchemist: Brotherhood                     [100] 春 2009  完結
      [████████████████]  64 / 64 eps  · completed 2024-01-14
                                                                                     [d bar, d meta: de-emphasized]
    ● Steins;Gate                                          [ 97] 夏 2011  完結
      [████████████████]  24 / 24 eps  · completed 2023-11-02
                                                                                     [d bar, d meta]

  ▌  jk move · / filter · l/enter detail · p/x/c/w/P status · X delete · r/u reset/undo · q quit
```

Notes:
- Section headers (`watching (4)`) are `text.primary` + bold. The count is
  `text.muted`.
- `─` rules between sections: `border.hair`.
- Focused row gets `▸` in `state.focus` and the row title in `state.focus` + bold.
- Completed rows use `text.dim` for both the bar and metadata; they've earned
  their de-emphasis.
- The resume indicator `[▸12]` in the row header is the episode the user will
  resume from: `state.now` + bold.
- **Not built (§11.3):** the row-1 right-meta drawn here (`[▸12] 冬 2024 放映中`,
  resume indicator + season + status chips) was the authored target and was never
  ratified. Shipped row 1 is title-only, with the episode count riding the row-2
  progress bar (§8.1); the count is never duplicated into row 1. Read the rest of
  this mock as the spec and that column as a comp.
- `l` or `Enter` from list focus moves to the detail pane (same grammar as
  Browse). At `w < 60` single-column, `l` is a no-op (no pane to move to), but
  `Enter` (or `Space`) opens the full-screen zoom, the only detail surface at this
  width.

### 5.4a History: Two-Pane

History uses the same two-pane grammar as Browse.

**Width tiers:**

The grid lives in **one of two places**: the in-pane view (`w ≥ PANE_SPLIT_MIN`,
i.e. any two-pane width) or the full-screen zoom (any width, roomier). Below
`PANE_SPLIT_MIN` there is no pane at all, so `Enter`/`Space` "drill toward the
grid" by opening the zoom directly:

| `w` | Layout |
|---|---|
| `w < 60` | Single-column list (§5.4). Clamp `active_pane = .list`. No pane to focus, so `Enter`/`Space` open the **zoom** directly (the only detail surface here). |
| `w ≥ 60` | Two panes. Detail = full detail pane with the interactive grid in-pane (narrow at the low end: `detail_w ≈ 25` at `w = 60` → ≈ 5 grid columns; ≈ 8 columns from `w = 100`). Pane toggle `h`/`l` works; `Enter` plays the focused episode; `Space` promotes to the roomier full-screen **zoom**. |

Episodes fetch on detail-pane entry at any two-pane width (`w ≥ 60`), so the
in-pane grid (or the zoom, if promoted) always has its data ready; below 60 the
fetch fires when `Enter`/`Space` open the zoom.

Empty / loading / error states (no focused record) fall back to the §5.4
single-column layout; no half-empty split. The split only engages when a record is
focused.

---

**History two-pane, list focused, 120 cols.** List: 45 cols. Detail: ≈70 cols.

```
                                                                                         [context: top bar, full width]
  SABIGOKU  ░  Watchlist  冬 2026                                               ·        [h1+bold fg] [d] right: [d]· dim (list focused)]
                                                                                         [spacer row]
  ▸ watching (4)                            [   COVER ART IMAGE               ]         [fg+bold header; detail pane: 20-col cover (detail_w≈70≥40)]
  ─────────────────────────────────────     [   or "no art yet" in d+italic   ]         [border.hair rule, list pane only]
    ▸ Frieren: Beyond Journey's End         [                                  ]         [focused row: bg.surface, f+bold title]
      [████████◐░░░░░░░]  6 / 28 eps        Frieren: Beyond Journey's End               [f+bold title in detail pane]
                                             放映中  冬 2024                             [h chip, m chip; omitted if null]
    ◐ Vinland Saga S2                       [--/100]                                     [d score placeholder]
      [░░░░░░░░░░░░░░░░]  0 / 24 eps        ─────────────────────────────────           [border.hair]
                                             28 eps · TV                                [m metadata]
    ○ Blue Period                           ─────────────────────────────────           [border.hair]
      [██████◐░░░░░░░░░]  5 / 12 eps         An elf mage who once defeated the           [m synopsis, word-wrapped to detail_w]
  ─────────────────────────────────────     Demon King now wanders the
                                             continent without purpose, until
  ▸ completed (12)                           she meets a young girl…
  ─────────────────────────────────────
    ● Fullmetal Alchemist: Brotherhood
      [████████████████]  64 / 64 eps

  ▌  jk move · / filter · l/enter detail · p/x/c/w/P status · X delete · r/u reset/undo · q quit                          [list focused; help per §7.5]
```

**History preview: detail stack (authoritative).** The detail pane renders
top-to-bottom as:

1. cover (or "no art yet")
2. **title**: the resolved primary title (`title_language`, §8.2, default
   `romaji`), bold
3. **first alt title**: the next form in `romaji → english → native` order minus
   the resolved primary; `fg2`; omitted when null or byte-equal to the primary
4. **second alt title**: the remaining form; `fg2`, additionally **italic when it
   is the native form** (foreign-language rule §1.3), otherwise plain; omitted
   when null or byte-equal to the primary
5. **score · genres**: `[--/100]` until known, then §2.2 tiers
6. hairline
7. **status + season/year chips**, or the `list_status` label when no chip resolves
8. **synopsis**, word-wrapped

Rows 3–4 mirror the Browse header's title stack (`draw_alt_titles`, §4.4/§8.2) so
the two surfaces stay consistent: same primary resolution, same alt order, same
per-field styling.

---

**History two-pane, detail pane focused, 120 cols.** Same geometry; `·` lights cyan.

```
  SABIGOKU  ░  Watchlist  冬 2026                                                   ·    [· is f (cyan): detail pane active]

  ▸ watching (4)                            [   COVER ART IMAGE               ]
  ─────────────────────────────────────     [                                  ]
    ▸ Frieren: Beyond Journey's End         [                                  ]         [selected row: bg.base, f title, ▸ dim]
      [████████◐░░░░░░░]  6 / 28 eps        Frieren: Beyond Journey's End
                                             放映中  冬 2024
    ◐ Vinland Saga S2                       [--/100]
      [░░░░░░░░░░░░░░░░]  0 / 24 eps        ─────────────────────────────────
                                             28 eps · TV
    ○ Blue Period                           ─────────────────────────────────
      [██████◐░░░░░░░░░]  5 / 12 eps        [1][2][3][4][5][6][▸7][8]                  [interactive grid; d watched, h▸ resume, m unwatched]
  ─────────────────────────────────────     [9][10][11][12][13][14][15][16]
                                            [17][18][19][20][21][22][23][24]
  ▸ completed (12)                          [25][26][27][28]
  ─────────────────────────────────────
    ● Fullmetal Alchemist: Brotherhood
      [████████████████]  64 / 64 eps

  ▌  hjkl scroll · h back · enter play · v provider · space zoom · q quit                   [detail pane focused; space promotes to zoom §5.3]
```

Notes:
- The detail pane uses `pane_split(w)` geometry (§3.2). At 120 cols: list_w=45,
  detail_w≈70. Cover tier from `detail_w`: ≥40 → 20-col cover.
- The focused entry drives the detail pane. Focus change = immediate update.
- The in-pane grid renders whenever the detail pane has focus, at every two-pane
  width (`w ≥ PANE_SPLIT_MIN` = 60, `active_pane = .detail`): narrower at 60–99
  cols (`detail_w ≈ 25` → ≈ 5 columns), roomier from 100 up. `PANE_SPLIT_MIN` is
  the only detail-surface threshold.
- `Enter` from a focused detail pane plays the focused episode, at any two-pane
  width; the grid is always in-pane there. `Space` (any two-pane width, and
  directly from the `w < 60` list) promotes to the roomier full-screen zoom
  (`active_view = .detail`, §5.3). `Esc`/`Space` demote back to the two-pane
  (`active_pane = .detail`) when there's room, else to the list (`w < 60`); `q`
  quits the app (Esc/Space/`h` own the demote, §7.4).
- Row 1 of a list entry is title-only at every width, so the title takes full
  `list_w`; the richer row-1 right-meta was never built (§5.4 note, §11.3).
- Null-degrade rules from §8.1 apply in full: `no art yet` in [d]+italic when the
  cover URL is null; `[--/100]` in [d] when score is null; `no synopsis yet` in
  [m]+italic when synopsis is null; chips omitted when null.
- The `28 eps · TV` row is the compact-line form of the metadata grammar (§5.3a):
  ~~`detail_w ≈ 70` at 120 cols is below `DETAIL_TWO_COL_MIN` (100), so the rail
  doesn't bloom here; it does once the pane clears 100 (`term ≥ 168`, below). The
  dedicated Provider/Pinned row (§5.3a) renders below this compact line; this mock
  keeps the two-field baseline for brevity.~~ This is the only form, at every
  width and origin; the provider row rides the top of the episode grid
  instead of the header (ROD-458). `v provider` in the help line cycles the
  provider pin; it is live on this in-pane surface, same as the zoom.

---

**Full-screen zoom from History, 160-col terminal (§5.3 geometry).**

At `w ≥ 160`, `detail_w ≈ 95` in the two-pane. After `Space` from detail pane
focus, the zoom gets the full canvas: `left_w ≈ 60`, `right_w ≈ 96`,
`cols ≈ 96 / 5 ≈ 19` grid columns.

```
  SABIGOKU  ░  Watchlist  冬 2026                                                                                 ·

  [   COVER ART   ]   Frieren: Beyond Journey's End
  [   20 × 7 cells]    放映中  冬 2024
  [               ]   ✦ [96/100] · Fantasy · Adventure · Drama
                      ────────────────────────────────────────────────────────────────────────────────────
                      28 eps · TV · Manga · 24 min · Madhouse · #12 rated 2024
                      ────────────────────────────────────────────────────────────────────────────────────
                       An elf mage who once defeated the Demon King now wanders the continent…
                      ▸megaplay ?senshi ?allanime · [v]
                      [▸1][●2][●3][●4][●5][●6][ 7][ 8][ 9][10][11][12][13][14][15][16][17][18][19]
                      [20][21][22][23][24][25][26][27][28]

  ▌  hjkl scroll · enter play · v provider · space/esc back
```

The two-column internal split (`left_w / right_w`) uses `DETAIL_TWO_COL_MIN = 100`,
gated on the zoom's own pane width (`body_w = term - 2`), practically `term ≥ 102`.
The same constant also gates the History persistent two-pane's split (§5.4a above),
but keyed to the narrower `detail_w` pane, so that surface needs `term ≥ 168` to
engage. At 160-col, `right_w ≈ 96` gives 19 grid columns. At 120-col zoom,
`right_w ≈ 72` gives 14 columns. This is where the zoom earns its keep over the
pane's ≈8 columns at 120 cols.

~~Clearing `DETAIL_TWO_COL_MIN` also blooms the metadata rail (§5.3a), but only
because this is a **History-origin** zoom, the one surface that sets `two_col =
true` unconditionally; a Browse-origin zoom at the same 160 cols still renders the
compact `28 eps · TV` line, plus its own dedicated Provider/Pinned row below it.
This mock keeps the `Episodes` / `Format` two-row baseline for brevity rather than
redrawing the full eight-row rail.~~ Metadata is the same compact line regardless
of origin (History or Browse) or width; `DETAIL_TWO_COL_MIN` gates only the
cover/content column split shown above. ROD-458.

### 5.5 Settings

Live-editable. Full width. No cover art.

```
  SABIGOKU  ░  Settings                                                          ·

  Player
  ─────────────────────────────────────────────────────────────────────────────────
  ▸ mpv path                      mpv                                  enter to edit
    default quality               best                                 hjkl to cycle
    translation                   sub                                  hjkl to cycle
    resume offset                 5s                                   hjkl to cycle
    skip mode                     both                                 hjkl to cycle

  Catalog
  ─────────────────────────────────────────────────────────────────────────────────
    metadata refresh              automatic                           [dim + italic]
    cover art cache               ~/.cache/sabigoku/covers            [dim + italic]
    provider                      allanime (default)                   hjkl to cycle

  Interface
  ─────────────────────────────────────────────────────────────────────────────────
    cover art                     [████ on ████]                     space to toggle
    kanji chips                   [████ on ████]                     space to toggle
    palette                       terminal_ghost                       hjkl to cycle
    transparent background        [████ off ████]                    space to toggle
    landing view                  history                              hjkl to cycle
    title language                romaji                               hjkl to cycle

  AniList Sync
  ─────────────────────────────────────────────────────────────────────────────────
    account                       not connected                       [dim + italic]
    connect                                                         enter to connect
    sync                          [████ on ████]                     space to toggle

  Updates
  ─────────────────────────────────────────────────────────────────────────────────
    version                       v0.1.1                              [dim + italic]
    check for updates             [████ on ████]                     space to toggle

  ▌  hjkl navigate · space toggle · enter edit · q save+quit
```

Five sections (Player · Catalog · Interface · AniList Sync · Updates): fifteen
interactive rows plus four read-only rows (two Catalog, one AniList Sync, one
Updates). The interactive-row split is Player `0..5`, Catalog `5..6` (the lone
`provider` row, below the two inert status rows), Interface `6..12`, AniList
Sync `12..14`, Updates `14..15`, pinned by a compile-time assertion in
`src/tui/view/settings.rs` so a future row insertion that shifts a boundary
breaks the build instead of silently misattributing a row to the wrong section
header.

Notes:
- Focused row: `palette.focus` + bold label over a `palette.bg_surface` row fill.
  Edit mode deepens the fill to `palette.bg_elevated` and switches the marker to
  `palette.hot` (magenta).
- Value under edit: `palette.fg` text with an inverted cursor block trailing the
  (append-only) edit buffer. The edit-mode help line reads `type to edit · enter
  confirm · esc cancel`.
- Toggle `[████ on ████]`: ON, fill and "on" text in `palette.focus`; OFF, the
  whole `[████ off ████]` widget in `palette.fg3` (dim).
- Section headers: `palette.fg` + bold, each followed by a full-width hairline rule
  in `palette.chrome`.
- Hint column (right): `palette.fg3`, right-anchored at `w-2-len` (ASCII-only, so
  the byte length matches the display width).
- **Catalog's status rows are read-only; `provider` is not.** `metadata refresh`
  and `cover art cache` render via `draw_inert_row` in `palette.fg3` + italic: no
  marker, no hint, and skipped by `j`/`k` navigation (they are not in
  `settings_rows`). The `[dim + italic]` annotation in the mock marks this
  treatment. `provider` is a real cycle row like `palette`, rendered below the
  status rows (status above controls, matching the AniList Sync section's order).
  `metadata refresh` reads `automatic` (stored rows are refreshed by a background
  task; a manual refresh is the `:sync` command, §6.3). `cover art cache` shows
  the cache path read-only. The path is **resolved at runtime** from the cache-dir
  helper + the `covers` subdir, so it honours `$XDG_CACHE_HOME`; the `$HOME`
  prefix is collapsed to `~` for display. The mock shows the default-home case; a
  custom `$XDG_CACHE_HOME` on another volume renders its real absolute path.
- **translation** cycles `sub`/`dub` (`config.translation`), default `sub`: the
  sub-vs-dub selector. The label is deliberately not "subtitle language": the row
  only ever drives the translation track, and providers expose a sub/dub
  translation type, not per-language audio tracks (so there is no separate "audio
  language" row either).
- **resume offset** cycles `0·3·5·10·15·30` seconds, displayed as `Ns` (e.g.
  `5s`), default `5s`.
- **skip mode** cycles `none·intro·outro·both`, default `both`.
- **palette** cycles `terminal_ghost·phosphor·nord·tokyonight`, default
  `terminal_ghost` (§1.4). Live-preview: cycling repaints on the next frame.
- **transparent background** toggles `config.transparent_background`, default **off**:
  the §1.4a remap of the `bg` tier to the terminal default. Live-preview like
  `palette`: flipping it re-resolves the active palette on the next frame.
- **default quality** cycles `worst · 480 · 720 · 1080 · best`, default `best`. It
  is honoured at stream-resolution time via a *cap* policy over the variants a
  provider exposes (`select_variant`): `best`/`worst` pick the resolution
  extremum; a rung picks the highest variant *at or below* it, falling back to the
  lowest available when every variant overshoots, so a capped user is never bumped
  over their ceiling, but always gets a playable stream. A direct path with no
  variants (a single fixed-resolution URL) ignores the setting; the preference is
  a silent no-op there, not a dead toggle.
- **provider** cycles unset → each registry provider name → unset (construction
  order; the first registered provider leads). Unset (`preferred_provider = ""`)
  means "follow the registry's construction leader" and displays as that leader
  tagged `(default)`, so it stays distinguishable from an explicit pin to the same
  provider (a pin holds if the leader ever changes; the default follows it), and
  the wheel keeps a real way back to unset. The preference governs only NEW
  canonical resolution (bind-time tie-breaks and fallback walk order). Fallbacks
  for rows with an unknown owner deliberately keep routing to the registry
  primary: a pre-existing binding never silently migrates provider. **Per-show
  override** is the `v` key on any detail surface (§5.3a ~~rail~~ / §7.5 keybinds): a
  separate `provider_pins` DB table keyed on canonical id, layered over this
  row's global setting only (effective preference = show pin, else global) and
  never writing back to it. The pin cycles the same construction order as this
  row (unpinned → each registry provider → unpinned); there is no
  Settings-surface control for the per-show pin, only the in-grid key.
- **landing view** cycles `history · browse · last_watched`, default
  `last_watched` (§8.3).
- **title language** cycles `romaji · english · native`
  (`config.title_language`), default `romaji`. A live-preview cycle row like
  `palette` and `landing view`: cycling it re-resolves every visible title on the
  next frame. Full spec, fallback chains, and governed surfaces: §8.2.
- **account** is read-only, like the Catalog status rows: `draw_inert_row`,
  `palette.fg3` + italic, not in `settings_rows`, skipped by navigation. Three
  states: the AniList user name once connected; `reconnect · token expired` when a
  token exists but was rejected; otherwise `not connected` (the mock's default).
- **connect** is the one `action`-kind row: Enter raises the connect modal
  (§5.5a) rather than editing a value. It renders through the same standard row
  path as every other row (marker, label, hint), but its value column is always
  empty.
- **sync** toggles `config.anilist_sync_enabled`, default **on**. This is the
  master switch for the whole sync rail (both push and pull); `sync_enabled()`
  ANDs it with `anilist_connected`, so `sync: on` with `account: not connected`
  (the mock's default state) is a real, expected combination: the switch is
  honoured, it just has nothing to gate yet. Flipping it off makes the rail inert
  without touching the stored token; flip it back on and sync resumes.
- **version** is read-only, `draw_inert_row` styling like `account`. The
  built-in version (`v0.1.1`), extended to `v0.1.1 (v0.1.2 available)` for the
  rest of the session once the boot check finds a newer release. It never
  claims "up to date": a silent check is indistinguishable from a failed one
  by design (06 §6.1), and this row must not pretend otherwise.
- **check for updates** toggles `config.check_for_updates`, default **on**:
  the gate on the boot check (06 §6.1). Purely a boot gate; flipping it on
  mid-session does not fire a check, the next launch does.

### 5.5a AniList Connect

The `connect` action row above raises a captured modal overlay
(`src/tui/view/connect.rs`) that runs the same OAuth loopback `sabigoku login`
uses, inline in the TUI: no shelling out, no losing the terminal screen.

**Borderless by design.** The modal is a `palette.bg_elevated` fill, full stop; no
`┌─┐│└┘` drawn around it. §3.1 (The Borderless Float System) treats box-drawing as
a last resort, never pane/overlay chrome, and §1.1 already names `bg.elevated` as
the palette's own "modal-ish overlay" elevation token. A drawn border would be a
second, redundant signal fighting the first. The mock below reflects that: it is
content only, no frame; the rectangle is implied by the `bg_elevated` fill in the
real render, not by anything drawn.

```
                            Connect AniList

               approve access in your browser to continue

                  browser didn't open? use this link:

     https://anilist.co/api/v2/oauth/authorize?client_id=NNNNN&redi
     rect_uri=http://127.0.0.1:8934/callback&response_type=token&st
     ate=9f2c7a1b4e6d0f3a5c8b1d2e4f6a7b9c

                       ⠠ waiting for approval… 4s

         no callback? run  sabigoku login --paste  in a terminal

                              c  copy link
                              esc  cancel

```

The bottom row (`no callback? run sabigoku login --paste in a terminal`) only
appears once the wait crosses 10s; shown here for completeness. Top-to-bottom this
is exactly the modal's row order: title → instruction → fallback caption → URL band
→ status → paste hint (own padded slot, present or not) → `c copy` / `esc cancel`,
plus a blank pad row above the title and a couple below the last hint.

Notes:
- **Captured overlay.** While the connect modal is open it owns every keystroke;
  nothing beneath it can be reached, and it draws last, on top of Settings, on a
  `palette.bg_elevated` fill so the obscured view can't be mistaken for still
  interactive. Only `esc` (cancel) and `c` (copy the authorize URL) do anything;
  `Ctrl-C` still hard-quits, matching every other in-flight worker in the app.
- **Hierarchy.** Title in `palette.hot` + bold. The one required action ("approve
  access…") in `palette.fg2`. The fallback path is visually subordinate: a
  `palette.fg2` italic caption introduces the authorize URL, which sits in its own
  `palette.bg_surface` inset band (not near-invisible dim text), since it is a
  real fallback action, not decoration. `c` always copies the *whole* URL
  regardless of how much of it is visible in the band. (The caption is `fg2`, not
  `fg3`: `fg3` and `chrome` share a hex in `nord`, so a `fg3` caption there would
  read as nearly invisible against the panel; `fg2` holds real contrast in all
  four palettes.)
- **Status line.** Spinner + `waiting for approval… Ns`. The spinner is
  `palette.focus` while fresh, escalating to `palette.hot` past a few seconds; the
  same slow-path convention the bottom bar and cover block use (§4.8),
  reimplemented locally against the modal's own clock rather than the shared one.
- **Paste-hint fallback (10s).** The loopback only completes when the browser can
  reach *this host's* `127.0.0.1:PORT`; a remote/SSH session's browser can't, and
  would otherwise sit on "waiting" forever with no way out but `esc`. Past 10s
  elapsed, a centred line points at the terminal fallback: `no callback? run
  sabigoku login --paste in a terminal`. It renders in `palette.warn` (amber), an
  attention register distinct from every other line in the modal (not `fg2`,
  which is the same register as the body text it would otherwise blend into; not
  `hot`, which the spinner above already escalates to past the slow threshold,
  and the two would compete for "the urgent thing here"). It gets its own padded
  slot, a blank row above and below, reserved whether or not the hint is showing,
  so the key hints below don't jump into a new position the instant it appears at
  10s.
- **Key hints.** Two centred `<key>  <action>` lines (the idiom the Browse/History
  absent states use), not one flat dim line; the key is `palette.focus` + bold so
  it actually reads as a key. Both hints' action text is `palette.fg2`,
  deliberately the same weight, since `cancel` is if anything the more critical of
  the two and must not read dimmer than `copy link`. `c` flips to `copied ✓` in
  `palette.fg` once used.
- **Too small to render.** Below roughly `28` cols or `10` rows the modal panel
  itself would read as clutter, so the modal instead draws one bare
  `connect: esc to cancel` line on a filled band; a mid-connect resize into a tiny
  terminal still tells the user the one key that works, instead of leaving
  Settings visually interactive while silently swallowing every key but `c`/`esc`.
- **The flow, at a spec level.** `connect` binds the loopback port and opens the
  browser **synchronously on the render path**, so a bind failure is a synchronous
  toast, never a half-open modal. The accept loop then runs off the render path
  (`login_loopback::await_connect`) so rendering is never blocked; `esc` sets a
  cancel flag and wakes the blocked `accept` by dialing the port once (a
  self-connect, the documented way to unblock a stuck `accept`), and teardown
  always joins the worker before freeing anything it touched. This is the same
  serve-connection core `sabigoku login` uses; the two entry points never diverge
  on CSRF handling or persistence, only on how they wait.

**Browser callback pages.** `src/anilist/login_loopback.rs` serves three small,
fully self-contained HTML pages (inline `<style>`, no external fonts/images/CDN;
they have to survive being served off a bare loopback listener) to whichever
browser tab is driving the flow, CLI or TUI:

| Page | Served when | Copy |
|---|---|---|
| `relay_page` | First landing: the token is in `location.hash`, which never reaches the server | "finishing sign-in" + a blinking `▌` (the app's own cursor motif, CSS `steps()`-timed so it's a hard cut, not a fade; §6.4's "no easing" rule holds even in a browser tab) |
| `done_page` | A state-valid `/callback` completed and persisted | "✓ signed in to AniList / you can close this tab" |
| `fail_page` | Bad state, no token, a rejected/unreachable verify, or a write failure | "sign-in didn't complete / check your terminal" |

Notes:
- **Theme-aware.** `@media (prefers-color-scheme: dark)` gives dark browsers a
  dark shell, the likely case for anyone running a terminal app, while the
  unstyled default stays a clean light card. The dark palette reuses §1.1's hex
  values verbatim (`bg.base`, `bg.elevated`, `border.hair`, `text.primary`,
  `text.muted`, `state.now`) so the tab reads as the same product, not a generic
  OAuth screen; light mode keeps the same accent identity but darkens it for
  contrast against a white card (`text.primary`'s neon green reads great on
  `bg.base`, badly on white).
- **Branding.** A small "錆獄 sabigoku" wordmark, the exact top-bar string (§3.4),
  above the headline on all three pages.
- **Security: the address-bar scrub.** `done_page` and `fail_page` both land on a
  URL still carrying the query string AniList/the listener put there
  (`/callback?access_token=…`). Both pages open with
  `<script>history.replaceState(null,'','/')</script>` **before** the stylesheet
  or body even parse, so the token clears the address bar and this tab's history
  entry as early as the page can manage it. `relay_page` does **not** get this
  script: its only job is `location.replace("/callback?" +
  location.hash.substring(1))`, handing the fragment to the server as a query
  string, and that line is the entire mechanism; do not alter or reorder it.
- **Shared, not duplicated.** All three pages pull their `<style>` block from one
  constant (`page_style`) rather than three copies, so a palette or contrast fix
  can't drift out of sync between them; the same "fix drift at the source"
  instinct as the §1.2 semantic aliases.

### 5.6 Loading / Startup

Full-screen loading state shown on a measurably slow startup. On startup the app
does two things: opens the local SQLite DB and loads history. It does not contact
AniList; search, the Discover feed, and metadata refresh are the network moments,
never startup.

```
  SABIGOKU  ░  Watchlist  冬 2026                                                 ·




                                      ⠙
                                 loading history                                    [m + italic, centered]




  [~]  opening local db…                                                            [f [~], m text]
```

Notes:
- Spinner: `state.focus`, centered in the viewport.
- Label below spinner: `text.muted` + italic.
- Bottom bar replaces the `▌` with `[~]` in `state.focus` while loading.
- If the DB opens and history loads fast (under ~200ms), skip this screen entirely
  and go straight to the landing view. The loading screen is only shown when the
  DB open is measurably slow (e.g., migration in progress on a large existing DB).
- **Slow threshold:** >3s shifts the spinner from [f] to [h] and the label updates
  to `taking a moment…` (§4.8).

### 5.7 Discover / Popular

Full-canvas card grid. 120-col terminal, large card tier (≥ 80 cols):
`slot_w = 22`, `cover_w = 20`, cover height adaptive (fills card width from cell
pixels; fallback 7 when unreported). 5 columns. Card 3 selected.

```
                                                                                         [context: top bar, full width]
  SABIGOKU  ░  Discover  冬 2026                                                 ·        [fg+bold name; chrome sep; f Discover tab (active); season chip m: selected card's season, absent if AniList returned null; f · always lit, single pane]
                                                                                         [spacer row]
  [1] Trending · [2] Popular · [3] Top Rated · [4] This Season                           [active=Trending f+bold; rest m; [N] keys m (active lifts to f); separator dots d]
                                                                                         [spacer row]
  [  COVER  ][  COVER  ][  COVER  ][  COVER  ][  COVER  ]                               [5 cover blocks; cover_h rows each; bg.surface fill; height adaptive (fills card width)]
  [         ][         ][         ][         ][         ]
  [  #1     ][  #2     ][  #3     ][  #4     ][  #5     ]                               [rank d, centered in cover placeholder]
  [         ][         ][         ][         ][         ]
  [         ][         ][         ][         ][         ]
  [         ][         ][         ][         ][         ]
  [         ][         ][         ][         ][         ]
  #1 TOP    [--]  #2     [72]  ▸#3 NEW  [85]  #4   [68]  #5   [--]                     [rank fg; score badge right-anchored at cover edge: [NN] tier-colour (91+ capped fg on cards); [--] d when null; TOP h+bold; NEW f+bold (suppressed on This Season); ▸ f gutter (x-1)]
  Frieren: B… FMA: Brothe Vinland S  Mob Psycho  Steins;Ga…                            [title fg; selected (#3) f+bold; clipped to cover_w with …]
  TV · 28ep ⚜♨  TV · 64ep ⚔   TV · 24ep ⚔   TV · 12ep ⚜    ─                          [format+eps m left-anchored, TV·Nep / Movie / TV·??ep; genre glyphs d right-anchored (§3.8a); ─ d when format null]
                                                                                         [gap row; slot_h = cover_h + 4]
  [PEEK CVR][PEEK CVR][PEEK CVR][PEEK CVR][PEEK CVR]                                    [peek row: tops of next card-row's covers clipped to leftover band (≥ 3 rows); no meta]

  ▌  hjkl move · enter open · P save · [ ] axis · / search · q quit
```

Notes:
- Top-bar strip: the `[D]iscover` tab is active (`state.focus` + bold) per §3.4;
  the mockup above shows the shorthand label. Season chip tracks the selected
  card's season+year, present as soon as the card is on screen (AniList's feed
  response arrives fully enriched, §8.6); absent (no cour fallback) only when
  AniList itself returned null; see §3.8.
- `·` dot: always `state.focus`; single pane, no dim state.
- Axis bar: the `[`/`]` and `1`–`4` keys drive it; the bar has no cursor of its
  own. Axis change triggers a refetch.
- The `▸` selection marker sits in the **left gutter at `x-1`** (one column left
  of the card's content origin) on the **rank row** (`y + cover_h`),
  `state.focus`, text-on-base. No box border, no background band around the card.
  The marker does not touch the cover cell, so cover art is never masked or
  composited.
- `TOP` is always rank #1 (`state.now` + bold). `NEW` is a current-cour show not
  ranked #1 (`state.focus` + bold), suppressed entirely on the `This Season` axis
  (every card there is already this-cour). The two are mutually exclusive.
- Score badge `[NN]` / `[--]`: right-anchored at the cover edge on the rank row.
  Tier colour per §2.2; the 91+ tier is capped at `text.primary` on cards
  (`state.now` is reserved for the `TOP` pointer). `[--]` in `text.dim` for null
  scores.
- Title clips to `cover_w` (20 cols at the large tier) with `…`. Selected title is
  `state.focus` + bold; unselected is `text.primary`.
- Format + episode count absent renders `—` in `text.dim` (not italic: factual
  placeholder, §1.3).
- Genre glyphs (§3.8a): up to 2 monochrome BMP symbols right-anchored at the cover
  edge on the format row, in `text.dim`. Absent for genre-unmapped cards. The full
  genre list is in the detail zoom pane.
- Gap row is part of `slot_h = cover_h + 4`.
- Peek row: the leftover vertical band below the last full card row (when ≥ 3 rows
  remain) shows the tops of the next card-row's covers clipped to that band. The
  `⠋ loading more…` / `all entries loaded` footer only renders when no peek row is
  visible. `all entries loaded` is `text.dim`, no italic (status fact, not
  annotation).
- `/ search` jumps to Browse and opens its search prompt.
- `Enter` opens the full-screen detail zoom (`active_view = .detail`,
  `detail_origin = .discover`), the shared detail-pane render pass unchanged.
  `Esc` returns to Discover. `P` saves to watchlist per the §4.10 path.

---

## 6. Interaction & Motion

### 6.1 Vim Navigation

| Key | Action |
|---|---|
| `h` | Move focus left (list pane → detail pane or vice versa) |
| `j` | Move cursor down in focused pane |
| `k` | Move cursor up in focused pane |
| `l` | Move focus right (list pane → detail pane, or expand detail) |
| `g` | Jump to top of list |
| `G` | Jump to bottom of list |
| `Enter` | Select item / enter detail / play episode |
| `Esc` | Peel one transient layer: close search/command, or exit detail/zoom to the list. Never switches base view. |
| `q` | Quit the app from anywhere. Normal mode only, so a literal `q` in a search/filter is text. Persists a dirty Settings tab first. |
| `/` | Open search prompt in bottom bar |
| `:` | Open command prompt in bottom bar |
| `B` | Switch to Browse view |
| `H` | Switch to History/Watchlist view |
| `D` | Switch to Discover view |
| `S` | Switch to Settings view |
| `v` | Cycle the open show's provider pin (detail surfaces only; §5.3a) |
| `r` | Recompute progress for selected show (History list pane only; no-op elsewhere) |
| `u` | Undo last status mutation (single-level, History list pane only) |
| `P` | "Plan it": add highlighted browse result to the watchlist as planning (Browse list pane) / set focused entry's status to planning, the 5th manual transition (History list pane) / save the selected card (Discover) |
| `p`/`x`/`c`/`w` | Status transitions in the History list pane (watching / dropped / completed / paused) |
| `X` | Arm the hard-delete confirm for the cursor-focused show (History list pane only, normal mode; §6.5) |

Pane focus is indicated by the `·` dot on the right side of the top bar:
`state.focus` color when the detail pane is active, `text.dim` when the list is
active (§7.3).

### 6.2 Search Interaction

1. User presses `/` from Browse view.
2. Bottom bar transitions from idle → search state (no animation, immediate).
3. Characters typed update the list filter synchronously if the result set is
   local (cached). If an AniList fetch is needed, show the `[~]` spinner in the
   bottom bar alongside the query.
4. `Esc` returns to idle state, restores the full list, clears the query.
5. `Enter` locks the search result set and moves keyboard focus to the list.
6. Subsequent `/` opens search again with the previous query pre-filled.

### 6.3 Command Line

`:` opens command mode. Recognized commands:

| Command | Action |
|---|---|
| `:q` | Quit application |
| `:dub` | Toggle dub/sub preference |
| `:sync` | Force a manual metadata refresh: re-fetch AniList metadata for items already in the local DB (the automatic background refresh covers the common case) |
| `:cache clear` | Clear cover art cache |

The `[~]` / `BTN_SYNC` glyph is reserved for the manual-refresh indicator; it is
not rendered for the automatic background refresh.

Unknown commands produce a `[!]` error toast and return to idle. The command line
does not persist history between sessions (can be added later).

### 6.4 Motion Principles

- **No transitions.** Terminal cell grids do not have smooth animation. State
  changes are immediate: no easing, no slide, no fade.
- **No exceptions.** The `▌` cursor was the one temporal effect (a terminal-side
  ~1hz blink, never a manual timer) until ROD-481 retired it; nothing has claimed
  the channel since (§4.9, §10).
- **Spinner frames** at ~100ms/frame are not "animation"; they are a progress
  signal. Use the braille sequence for minimum visual noise.
- **Cover art loading:** image appears immediately when data is available. No
  crossfade. The spinner is removed and the image cell block is written in one
  draw cycle.
- **Cover preview settle:** in Browse and History, the cover fetch is debounced by
  a 150ms cursor-settle. Title and metadata text update instantly on cursor move;
  the cover image trails by roughly 150–250ms. Discrete navigation (pane/view
  switch) syncs the cover immediately. This removes per-row flicker on fast
  scrolling without hiding any metadata.
- **List filtering:** synchronous, no debounce at the UI layer. If the underlying
  search is async (AniList), show `[~]` in the bottom bar while results are
  pending. The existing visible results remain until new ones arrive; no flash to
  empty.
- **Focus changes:** immediate. No cursor animation.

### 6.5 Hard-Delete Confirm

`X` (shift+x) in the History **list** pane, normal mode, arms a hard-delete
confirm on the cursor-focused show: a no-op if the list isn't focused or the
cursor names no entry. Lowercase `x` is untouched (still `.dropped`, §6.1), a
distinct codepoint, so there is no collision.

Arming replaces the bottom bar with the §4.2 confirm prompt: a fourth
bottom-command-line mode alongside idle/search/command, reusing the Settings
edit-mode hijack pattern (§5.5). The list pane stays visible but **frozen**.
Cursor keys, pane switches, and view switches (including `q`) do not reach it
while armed. The `▌` is suppressed for the same reason it is suppressed in
search: the `[!]` glyph takes its position.

**Key handling while armed:**

| Key | Effect |
|---|---|
| `y` / `Y` | Execute the delete |
| `X` (repeat) | No-op, stays armed. A key-repeat storm can't self-confirm. |
| `Ctrl-C` | Still hard-quits (emergency exit, unchanged) |
| `Esc`, `Enter`, anything else | Cancel, return to idle |

`q` does **not** quit while armed. It falls into the "anything else" cancel path
and is swallowed, matching the "any non-y cancels" reading (§10 logs this call).
Deleting executes a cascading `DELETE` (episode history and cache follow via
existing FKs), drops the row from the in-memory list, and holds the cursor on its
ordinal (now the next row) unless the deleted row was last, in which case it steps
back one; deleting the only remaining show falls to the §8.3 empty state. A stale
single-level undo (`u`, §6.1) pointing at the deleted row is cleared. Deleting the
currently-playing show is refused with a warn toast: the TUI never kills mpv out
from under itself.

**Frozen list, reload-cancels:** a background history reload (post-play
reconcile) reorders the underlying list, so any armed confirm is silently
cancelled the moment a reload lands. The stored row index can no longer be trusted
to name the row the prompt is showing. This is a correctness guard, not a UX
nicety: firing a stale-indexed delete would destroy the wrong show's history.

---

## 7. View System & Focus Model

This section is the implementable specification for view switching, the per-view
focus model, the B/H/D/S view-switch binds (with F1–F4 aliases), bottom-bar help
strings, and the Esc chain. Everything here is a concrete buildable decision. An
implementer should need zero additional design calls to implement `active_view`,
`active_pane`, and the keybind dispatch below.

### 7.1 Views

Sabigoku has four base views plus the full-screen Detail zoom as a fifth
`active_view` value. They share the same top-bar / bottom-bar chrome and the same
`bg.base` void background. They differ in content layout and available keybinds.

| View | Identifier | Default | Layout |
|---|---|---|---|
| Browse | `active_view = .browse` | Optional (config `landing = "browse"`, §8.3) | Two-pane: list column + detail column (§3.2). `w < 60` collapses to list only. |
| History | `active_view = .history` | Default (the `landing` config lands here directly on `"history"`, and via the resume open on the shipped `"last_watched"` default, §8.3) | Two-pane: list + detail, identical grammar to Browse (§5.4a). `w < 60` collapses to list only. |
| Detail | `active_view = .detail` | No | Full-screen zoom: detail + episode grid (§5.3). Reached with `Space` from a focused detail pane in **Browse or History**, or `Enter` from a Discover card, at any width; or directly from the History list at `w < 60` (no pane to focus). The universal grid surface: the in-pane detail also carries its own (narrower) grid from `PANE_SPLIT_MIN` up, so `Enter` there plays instead of promoting. |
| Discover | `active_view = .discover` | No | Single-pane: full-canvas card grid (§3.8, §5.7). No `active_pane` semantics. Reached with `D` or `F3` from any view. |
| Settings | `active_view = .settings` | No | Single-pane: full-width settings rows (§5.5) |

**`.detail` is both an `active_pane` value within Browse/History and a standalone
`active_view` (the zoom).** Browse's and History's right-hand detail *pane*
(§7.3, reached with `l`/`Enter`) is the default "triage scrub" surface. The
standalone Detail view is the full-screen zoom (§5.3), reached with `Space` from a
focused detail pane in either Browse or History at any two-pane width
(`w ≥ PANE_SPLIT_MIN`). `detail_origin` records the entry point (`.browse`,
`.history`, or `.discover`). `Esc` from zoom demotes back to the two-pane with
`active_pane = .detail` (`Space`/`h` do the same). `q` does not back out; it quits
the app. See §7.4 for the full Esc chain.

Browse can be selected as the landing view (`landing = "browse"`, §8.3), but it is
catalogue *search*: there is no feed to populate it, so a Browse landing opens on
its idle search prompt. The popular feed lives in the separate **Discover** view
(§8.6).

### 7.2 View Switching Keybinds

#### Primary binds (vim-native, single-key)

Four destinations, each with one vim-native letter. All are normal-mode only (a
literal letter in a search/filter appends instead of switching), and each is a
no-op if already on that view.

| Key | Action | From |
|---|---|---|
| `B` | Switch to Browse | Any view (normal mode) |
| `H` | Switch to History/Watchlist | Any view (normal mode) |
| `D` | Switch to Discover | Any view (normal mode) |
| `S` | Switch to Settings | Any view (normal mode) |

Each is a **direct go-to, not a toggle**. `H` from History is simply a no-op
(press `B` to go to Browse); no letter doubles as a toggle.

Leaving Settings via any of these persists a dirty tab (`leave_settings`). `q`
quits the app (persisting first); `Esc` does **not** leave Settings; it is a no-op
there.

**Entering the standalone Detail zoom** is not a view-switch keybind; it is a
promote. `Space` from `active_pane = .detail` opens `active_view = .detail` at any
two-pane width in either Browse or History; no width gate; the zoom is always
available as the roomier grid. `Enter` from a focused detail pane plays the
focused episode instead, at any two-pane width; the in-pane grid is present from
`PANE_SPLIT_MIN` up. At `w < 60` there is no pane, so `Enter`/`Space` from the
History list open the zoom directly. `Esc`/`Space` demote back to the two-pane
(`active_pane = .detail`) when there's room, else to the list; `q` quits (§7.4).
`Enter`/`l` from the list step into the in-view detail *pane* first (§7.3)
whenever there is one (`w ≥ 60`).

#### F-key aliases (discoverable navigation)

F-keys are secondary aliases for the primary letter binds: same destinations, in
the same order, content views (F1–F3) ahead of the meta view (F4). They are kept
so a new user mashing function keys still lands somewhere sensible. Unlike the
letters, they are **global**: they fire in any mode (they can't be typed into a
search), so no normal-mode guard.

| Key | Action |
|---|---|
| `F1` | Switch to Browse (= `B`) |
| `F2` | Switch to History (= `H`) |
| `F3` | Switch to Discover (= `D`) |
| `F4` | Switch to Settings (= `S`) |

Each is a no-op from its own view. In crossterm these arrive as
`KeyCode::F(1)`–`KeyCode::F(4)`; match them in the key handler exactly like any
named key.

The **top-bar tab strip** (§3.4) is the discovery surface for the view letters: it
shows `[B]rowse · [H]istory · [D]iscover · [S]ettings` persistently, with the
bracketed letter on each tab. The F-keys remain as a quiet fallback. The
bottom-bar help lines do not carry the view keys; the strip is the one canonical
place for them (§7.5).

### 7.3 Focus Model

#### What "focus" means

Focus is which pane currently receives keyboard input. It has two dimensions:

1. **View-level focus:** which view is displayed. Controlled by the view-switch
   keybinds (`B`/`H`/`D`/`S`, `F1`–`F4`).
2. **Pane-level focus:** within a multi-pane view, which pane is active.
   Controlled by `h` and `l`.

In Settings (single-pane), pane-level focus is always `.list` and does not change.
There is no second pane to move to.

In Browse and History (`w ≥ 60`), pane-level focus switches between `.list` (left)
and `.detail` (right) via `h` / `l`. At `w < 60`, `active_pane` is clamped to
`.list`; only one column is rendered.

#### The `·` indicator (§3.4)

The `·` dot rendered right-aligned in the top bar marks pane-level focus.

| View | `active_pane` | `·` color |
|---|---|---|
| Browse | `.list` | `color.fg3` (dim: list is the default, no emphasis needed) |
| Browse | `.detail` | `color.focus` (cyan: detail pane is explicitly selected) |
| History | `.list` | `color.fg3` (dim, symmetric with Browse list) |
| History | `.detail` | `color.focus` (cyan, same as Browse) |
| Detail (zoom, `active_view = .detail`) | - | `color.focus` (the full-screen zoom is focused) |
| Discover | - (single pane) | `color.focus` (always lit; no dim state, §3.8) |
| Settings | `.list` (only value) | `color.focus` |

The `·` uses cyan only, never magenta (§10). It is always rendered. It does not
disappear in single-pane views. Its persistent presence at a fixed right-aligned
position is the anchor that makes the top bar feel stable across view transitions.

Top bar rendering by view: a four-tab strip after `░`, plus a season/year add-on
chip after the strip. The strip's active tab and the season chip are differentiated
from the rest by color, no separator glyph beyond the `·` dots:

| `active_view` | Active tab (`color.focus`, label bold) | Season chip (`color.fg2` / text.muted) |
|---|---|---|
| `.browse` | `[B]rowse` | selected show's season+year, else current cour |
| `.history` | `[H]istory` | selected show's season+year, else current cour |
| `.detail` | inherits `detail_origin` (`[B]rowse` \| `[H]istory` \| `[D]iscover`) | focused show's season+year only; **no** cour fallback |
| `.discover` | `[D]iscover` | selected card's season+year when known; absent if null, no cour fallback (§3.8) |
| `.settings` | `[S]ettings` | - (none) |

The full strip occupies cols 18–63; the season chip sits two cells after it (col
66) and drops first under width pressure (w < 78). Below w = 66 the strip
abbreviates to `[B] · [H] · [D] · [S]`; the abbreviated strip and the `·` always
survive (§3.4). The inactive tabs render `text.muted` labels with `text.muted`
bracket keys (the key is the hint being taught; `text.dim` buries it against
bg_base). The season chip is `text.muted` so it reads distinct from the cyan
strip, matching how season/year reads in History rows (§5.4).

#### `h` / `l` behavior by view

| View | `active_pane` | `h` | `l` |
|---|---|---|---|
| Browse | `.list` | no-op (already leftmost) | set `active_pane = .detail` |
| Browse | `.detail` | set `active_pane = .list` | no-op (already rightmost) |
| History (`w ≥ 60`) | `.list` | no-op (already leftmost) | set `active_pane = .detail` |
| History (`w ≥ 60`) | `.detail` | set `active_pane = .list` | no-op (already rightmost) |
| History (`w < 60`) | `.list` (clamped) | no-op | no-op |
| Settings | `.list` (only) | no-op | no-op |

History has identical `h`/`l` pane-toggle behavior to Browse when `w ≥ 60`. At
`w < 60`, History collapses to single-column and `h`/`l` are silently consumed.
`j`/`k` navigate the focused pane's content in all views. `h` from the zoom
demotes (§7.4).

### 7.4 Esc Chain

`Esc` behavior is context-dependent. This table is exhaustive: every
`(active_view, input_mode, active_pane)` combination that needs a non-trivial Esc
action is listed. Everything not listed is a no-op.

| View | `input_mode` | `active_pane` | `Esc` action |
|---|---|---|---|
| Any | `search` | any | Close search prompt. Restore full list. Set `input_mode = .normal`. Stay in current view. |
| Any | `command` | any | Close command prompt, set `input_mode = .normal`. |
| Browse | `normal` | `.detail` | Set `active_pane = .list`. (Return focus to list, same as `h`.) |
| Browse | `normal` | `.list` | No-op. `q` handles quit from Browse. Esc does not quit. |
| Detail (zoom) | `normal` | - | **Demote:** `active_view = detail_origin`; `active_pane = .detail` when there's room for the pane (`w ≥ 60`), else `.list` (the zoom was opened from a single-column list at `w < 60`). From a Discover-origin zoom: back to `.discover` (no pane to restore). `Space` and `h` have the same effect (zoom toggle / back). |
| History (`w ≥ 60`) | `normal` | `.detail` | Set `active_pane = .list`. (Return focus to list, same as `h`.) |
| History | `normal` | `.list` | **No-op.** Esc peels transient layers only; base-view switches go through `B`/`H`/`D`/`S` (or `F1`–`F4`). `q` quits. |
| Settings | `normal` | `.list` | **No-op.** Same as History: Esc does not leave Settings. `q` quits (persisting a dirty tab); the view letters switch away (also persisting). |
| Settings | `edit` (field under edit) | `.list` | Cancel field edit. Return to Settings normal. `input_mode` stays `.normal`; the edit buffer is discarded. |

**`q` vs Esc:** `q` quits from anywhere in normal mode, full stop; the layered
peel belongs to `Esc`/`Space`/`h`. In search/filter mode, `q` is appended to the
query as text (the `input_mode` guard sends it to the search handler).

| `input_mode` | `q` action |
|---|---|
| `normal` | Quit, from any view/pane. Settings persists a dirty tab first (`leave_settings` → save-if-dirty). `q` never navigates. |
| `search` | Not a quit: `q` is query text. |

**Why Esc does not quit from Browse normal:** `q` is the quit key throughout
(§6.1). Esc-as-return is the vim idiom. In Browse list normal with no modal open,
there is no level back, so Esc is a no-op rather than a quit trigger.

**Why Esc from History/Settings (.list) is a no-op:** Esc means "peel one
transient layer," never "switch base view." Over a base-view list there is no
layer to peel, so Esc does nothing: History stays History, Settings stays
Settings. Base-view changes are explicit: the `B`/`H`/`D`/`S` letters (and their
`F1`–`F4` aliases).

**Why zoom Esc lands on `.detail`, not `.list`:** the user arrived at zoom via the
detail pane. Esc undoes one step. Skipping back to list would be jarring,
especially on a long-runner the user was navigating. The exception is `w < 60`,
where there is no pane to land on, so Esc returns to the single-column list.

### 7.5 Bottom Bar Help Strings

The help line is the idle state of the bottom bar (§3.5 State 1). It updates per
view. The `▌` and the rendering rules from §3.5 are unchanged; only the text
content varies.

Each per-view help line is modelled as an array of styled segments: keybind
characters render `color.fg2` + bold (§1.3), surrounding words render `color.fg3`.
The `▌` uses `color.hot`, steady as always (§4.9). Rendered by the idle-help
branch of the bottom bar in `src/tui/chrome.rs`.

**Character budget:** at 80 cols, the help line has ~74 chars after the `▌` and
its padding. The strings below are written to fit that budget.

#### Browse: normal, list pane focused

```
  ▌  hjkl · / find anime · P save · q quit
```

Bold keybinds: `h`, `j`, `k`, `l`, `/`, `P`, `q`.

#### Browse: normal, detail pane focused

```
  ▌  hjkl scroll · h back · enter play · v provider · space zoom · q quit
```

Bold: `h`, `j`, `k`, `l`, `h`, `enter`, `v`, `space`, `q`.

Note: `q` quits the app; `h`/`Esc` return focus to the list. Browse uses this
string at all two-pane widths (`w ≥ 60`); `enter play` and `space zoom` are always
present. The in-pane grid renders at every two-pane width (narrower at
`60 ≤ w < 100`: `detail_w ≈ 25` at `w = 60` → ≈ 5 columns); `Enter` plays the
focused episode and `Space` promotes to the full-screen zoom. `v` cycles the open
show's provider pin (§5.3a); the pin has no word on the provider row, so the bold
token there is the pinned provider and this hint is the only place the key is
named. Episodes load on detail entry, not on list hover
(scrolling Browse never fires a fetch; parity with History). At 80 cols the string
fits comfortably within the ~74-char budget: it is 68 characters.

#### History: normal, list pane focused

```
  ▌  jk move · / filter · l/enter detail · p/x/c/w/P status · X delete · r/u reset/undo · q quit
```

Bold: `j`, `k`, `/`, `l`, `enter`, `p`, `x`, `c`, `w`, `P`, `X`, `r`, `u`, `q`.

Note: the view keys are NOT in this line; the top-bar tab strip (§3.4) carries
`[B]rowse · [H]istory · [D]iscover · [S]ettings` persistently, so the bottom bar
spends its width on view-specific actions instead. `/ filter` and `l/enter detail`
are shown explicitly: History shares Browse's pane grammar, and its local filter
(distinct from Browse's catalogue search, §8.4) isn't obvious in a watchlist
without the hint. Over budget at 80 cols, so the tail clips; `/ filter` sits near
the front to survive it. `X delete` sits after the `p/x/c/w/P status` cluster, the
same destructive-adjacent grouping the status keys already occupy.

#### History: normal, detail pane focused (any two-pane width)

```
  ▌  hjkl scroll · h back · enter play · v provider · space zoom · q quit
```

Bold: `h`, `j`, `k`, `l`, `h`, `enter`, `v`, `space`, `q`.

Identical to Browse detail pane focused: symmetric two-pane grammar, including
`v`. One string at every two-pane width; the hint does not vary within History's
two-pane range.

#### Detail (zoom): normal

```
  ▌  hjkl scroll · enter play · v provider · space/esc back
```

Bold: `h`, `j`, `k`, `l`, `enter`, `v`, `space`, `esc`.

`space/esc back` reinforces that both keys demote from zoom. `v` cycles the
provider pin; the zoom is a detail surface like the in-pane grid. `q` quits the
app; it is not shown, keeping the line within budget.

#### History: empty (no records)

```
  ▌  D discover · B browse · q quit
```

Bold: `D`, `B`, `q`.

This is the §8.3 empty state. Minimal help: the `/` filter is suppressed (nothing
to filter), and the screen itself names the state and points first to Discover
(`nothing watched yet` / `D see what's popular` / `B search for a show`). `D`
leads `B` to match the absent state's priority order.

#### Settings: normal

```
  ▌  hjkl navigate · space toggle · enter edit · q save+quit
```

Bold: `h`, `j`, `k`, `l`, `space`, `enter`, `q`.

Settings persists a dirty tab on the way out, so `q` reads `q save+quit` (the `+`
signals one press does both). Leaving without quitting goes through the top-bar
strip letters (`B`/`H`/`D`), which also persist; `S` is a no-op inside Settings.
`Esc` is a no-op here; the field-edit cancel lives in the edit-mode line below.

#### Settings: field under edit

```
  ▌  type value · enter confirm · esc cancel
```

Bold: `enter`, `esc`.

The `▌` is suppressed in this mode; the field edit cursor takes that visual
slot. However this help string still displays to confirm what keys are available.
The `▌` reappears when the edit is committed or cancelled.

#### Discover: normal

```
  ▌  hjkl move · enter open · P save · [ ] axis · / search · q quit
```

Bold: `h`, `j`, `k`, `l`, `enter`, `P`, `[`, `]`, `/`, `q`.

`hjkl` navigate the card grid (left/right wrap within a row; up/down move
card-rows). `enter` opens the detail zoom (`active_view = .detail`,
`detail_origin = .discover`). `P` saves the selected card to the watchlist per the
§4.10 path. `[`/`]` cycle the active axis (`Trending` → `Popular` → `Top Rated` →
`This Season` and back); `1`–`4` select directly (the axis bar annotates these in
place). `/` jumps to Browse and opens its search prompt; there is no in-view
filter in Discover. The view keys live in the top-bar tab strip (§3.4), not this
line. `q` quits.

At 80 cols this string is 64 chars, within the ~74-char budget.

#### Any view: search active (§3.5 State 2 unchanged)

The bottom bar becomes the search prompt. The help string is replaced by the live
query display.

#### Any view: command active (§3.5 State 3 unchanged)

The bottom bar becomes the command prompt.

---

## 8. Data Reality: AniList-first, degrade-by-design

This section governs how the Terminal Ghost chrome specified in §§1–7 renders when
a field it assumes is null.

**The model.** AniList GraphQL is the catalog, search, discovery, and metadata
brain. A Browse search or a Discover feed page returns the full metadata shape in
one response (the shared `GQL_FIELDS` selection): romaji/english/native titles,
cover URL, score (integer 0–100), status, season/year, genres, format, episode
counts, synopsis, source, duration, studios, rankings, `nextAiringEpisode`,
`countryOfOrigin`, `id`/`idMal`. There is no post-search enrichment pipeline;
results arrive complete, and per-field gaps are genuine AniList nulls (unannounced
titles, obscure entries), not a transient pre-enrichment state.

Streams and episode lists come from a **multiprovider registry**: multiple
concrete `StreamProvider` implementations behind one trait, a day-one
architectural commitment, not a future extension. AllAnime is the first provider
implemented, with further concrete providers following immediately; every rule in
this section (bindings, absences, pins, fallback walks, the serving marker)
assumes N providers with per-provider availability. Providers supply exactly two
things: **episode lists** and **stream URLs**. They contribute no catalog
metadata.

The **datastore keys shows by the AniList canonical id from day one.** Provider
bindings, provider absences (`provider_absences`, 7-day TTL), and per-show
provider pins (`provider_pins`) all hang off that canonical row via FK. Persisted
metadata columns are nullable and upserted with `COALESCE` so a null re-fetch
never clobbers a stored value (the DB-safety rule; §5.3a Studios notes the
per-column application). A background **metadata refresh** re-fetches AniList data
for stored rows so airing status, countdowns, and scores stay current; `:sync`
(§6.3) forces it manually.

**The strategy.** The full Terminal Ghost chrome renders from whatever is present;
null fields render in explicit, consistent degrade states. The UI looks
intentional, not broken: a screen with partial data reads clearly rather than
crashing or showing a wall of blanks. The degrade states vanish *per-anime* the
moment data arrives; no code changes at the call sites.

### 8.1 Data Availability Matrix

What each surface renders, and from where. A field renders when it has a value and
falls back to the degrade rendering below when that particular anime has no value.
The degrade rules are per-anime fallbacks, not a global empty state. Degrade
tokens reference §1.2 aliases.

| Surface · Field | Source | Rendered when present | Fallback when missing |
|---|---|---|---|
| **Browse · score** | AniList `averageScore` | compact `[NN]` badge, right-anchored against the pane edge, §2.2 tier colour | `[--]` in list-row dim (`fg3`); the episode count seats to the badge's left on a wide pane (title > score > eps) |
| **Detail · title + alt titles** | AniList `title{romaji english native}` | resolved primary bold (`title_language`, §8.2, default `romaji`), then the other two forms as alt rows in `romaji → english → native` order minus the primary, shown when present and distinct from the primary (`draw_alt_titles`) | never blank: the fallback chain (§8.2) backstops to romaji; an absent alt row is simply skipped, never rendered empty |
| **Detail · status chip** | AniList `status` | kanji status chip (§2.3) | omitted; no empty chip or placeholder span |
| **Detail · season chip** | AniList `season` / `seasonYear` | `冬 2026`-style chip when both season and year are known | omitted; never an empty chip |
| **Detail · score line** | AniList `averageScore` | `[NN/100]`, `✦` prefix when ≥ 91 | `[--/100]` in `[d]` |
| **Detail · genres** | AniList `genres` | ` · Genre · Genre` appended to the score line | omitted; no row, no `·` separator |
| **Detail · cover art** | AniList `coverImage` | the §3.3 cover image (Kitty / half-block) | `no art yet` in `[d]` + italic when the URL is null; the block keeps its reserved cell dimensions |
| **Detail · metadata line**~~/rail~~ | AniList `episodes` / `format` / `source` / `duration` / `studios` / `rankings`; DB `provider_pins`; DB binding rows + `provider_absences` + live `episodes.for_source` | `detail_meta_fields()` (§5.3a) emits the ordered six-field list, rendered on the one compact `N eps · kind · …` line at every width and origin (Rank last, sheds first) ~~or the `Label  Value` rail (two-column surfaces, all eight)~~; provider and pin are not fields and instead draw `provider_line` ~~, the compact form's dedicated Provider/Pinned row beneath the joined line~~ , a dedicated row atop the episode grid, straight off the session (ROD-458, ROD-484) | Episodes is the floor: `? eps` ~~/ `Episodes  ?` in `[d]`~~ when no count is known, never omitted, so the line is never empty. Format/Source/Duration/Studios/Rank each omit outright when null; no orphan `·`~~, no bare rail row~~. The provider row omits outright when the show has no canonical identity, otherwise always renders (even all-`?`), dimming only when every provider is unchecked. The pin is a bold boost on its own token, so an unpinned show simply has no bold token, and a pin on a ~~retired or unconfirmed~~ provider outside the registry shows nothing (§5.3a, ROD-524: unconfirmed tokens keep the boost). The `nextAiringEpisode` countdown renders on the **chips row** (§4.4) instead: a live signal, not a stored snapshot |
| **Detail · synopsis** | AniList `description` | word-wrapped synopsis | `no synopsis yet` in `[m]` + italic |
| **History · row meta** | DB `progress`, `total_episodes`, `list_status` | row 1 is title-only; the episode count renders on the row-2 progress bar, not duplicated in row 1 (the richer row-1 right-meta was never built, §5.4/§11.3) | count degrades to `N / ? eps` on the bar when `total_episodes` is null |
| **History · progress bar** | DB `progress`, `total_episodes` | bar proportional to `progress / total_episodes`, with `N / M eps` | `N / ? eps`; the bar fills to ⅓ width as a non-zero signal when total is null |
| **History · season chip** | - | not rendered in rows | the history row is title + progress bar + meta; no chip (the detail pane carries the chips) |
| **History · score badge** | - | not rendered in rows | the `[NN]` badge is omitted; the space is reclaimed by the title |
| **Episode grid** | provider `episodes()` live fetch | the episode-label grid | loading spinner during fetch; centered `no episodes` absent-state when a provider genuinely returns zero. A provider with no listing endpoint mints positional labels from the canonical AniList count instead (`domain::expected_episode_count`) |

Browse / search list rows, History rows, and Discover cards render the **resolved
primary title** (`title_language`, §8.2) rather than a hardcoded romaji field.

There is **no watchlist status glyph** on Browse / search rows: History rows come
from the local store, so their watch-state is already loaded (hence History's
status chips), but Browse results come from AniList over the network and carry no
watch-state; a glyph there would need a per-row local-DB (or cache) lookup the
search path doesn't otherwise do. Terminal Ghost keeps the search path fast (§10).

**Score fallback.** `[--/100]` (detail pane) / `[--]` (list rows, §2.2) is the
fallback when the score is null; it does not participate in the §2.2 score-tier
rules (those apply to real integer scores only). A null score is not a score of 0.

**Cover fallback.** The `no art yet` state (rendered when no cover URL exists) is
distinct from the §4.8 loading spinner (an in-flight fetch). The two must not be
conflated in code: the spinner means "fetching", `no art yet` means "nothing to
fetch".

**History row meta.** A History entry is two physical rows: row 1 is the **title
only**, row 2 is the §4.5 progress bar carrying the episode count
(`[████░░]  N / M eps`; `N / ? eps` when `total_episodes` is null). The count is
**not** duplicated into a row-1 meta column: it already rides the bar, and the
status is already carried by the group header plus the row glyph. §5.4's richer
row-1 right-meta (resume indicator `[▸N]`, season chip, status kanji) was never
ratified and never built (§11.3); the no-duplication rule is the standing one
(§10).

### 8.2 Title Language Preference

Every surface that renders "the anime's title" as a single string resolves it
through `title_language`: a config setting, three values, default `romaji`, with a
fallback chain so a resolved title is **never blank**:

| `title_language` | Fallback chain |
|---|---|
| `romaji` **(default)** | romaji → english → native |
| `english` | english → romaji → native |
| `native` | native → romaji → english |

Romaji is the field AniList reliably supplies, so it is the common backstop at the
end of all three chains: whichever preference is active, a title falls through to
romaji before it can ever render blank. (If even romaji is somehow an empty
string, a defensive `"—"` placeholder in the detail render info is the last-resort
backstop beneath the chain.) There is deliberately **no fourth "Auto" value**:
every option already resolves as "preferred-with-fallback," so `english` already
behaves as the English-preferred/Auto case a separate value would offer (§10).

**Never-blank invariant.** No surface this setting governs may render an empty
title string. The chain exists precisely so a null or empty english/native form
never surfaces as blank text; it silently falls through to the next field, ending
at romaji (and `"─"` beneath that).

**Where it lives.** `title_language` is a cycle row in Settings → Interface
(§5.5), after `landing view`. Like `palette` and `landing view`, it is an
immediate-effect toggle: no restart, no explicit apply step. Cycling it
re-resolves and re-renders every visible title on the very next frame.

**What it governs**: every surface that renders the anime's title as a resolved
single string:

- Browse list rows (`src/tui/view/browse.rs`)
- History list rows (`src/tui/view/history.rs`)
- Detail pane title line: the bold primary in the header (§4.4) and the History
  preview stack (§5.4a)
- Discover feed card rows (`src/tui/view/discover.rs`, §3.8 card anatomy)

**Explicitly excluded: the top bar (§3.4).** `SABIGOKU` is app branding, not a
show title, and is unaffected regardless of `title_language`.

**Toast copy (§4.7/§4.10).** No toast string in the §4.10 matrix embeds an anime
title; every event's copy is title-free. The invariant still binds prospectively:
if a future toast's copy embeds a title, it must resolve through this same chain
rather than defaulting to romaji.

**Data shape.** All three forms are fetched and persisted:

| Form | Domain field | Stored field |
|---|---|---|
| Romaji | `name` | `title` |
| English | `english_name` | `title_english` |
| Native | `native_name` | `native_name` |

**Alt-row generalization.** The detail title+alt stack (`draw_alt_titles`,
§4.4/§5.4a) generalizes the same way the primary line does: the two alt rows are
whichever two forms are *not* the resolved primary, in `romaji → english → native`
order, each skipped when null **or** byte-equal to the resolved primary.
"Resolved," not "configured": if a preference falls back (e.g. `english` with a
null english form resolves to romaji), the alt-row order is computed against the
field that actually rendered as primary, so the fallback target is never
duplicated into its own alt row. The de-dupe check applies symmetrically to both
alt slots against whichever form is primary. Per-field italic styling is unchanged
by this generalization (§1.3, §4.4): native is italic whenever it is an alt row,
romaji and English never are.

### 8.3 The Landing View: History Is the Floor

The landing view is a config setting (`landing`, surfaced as the Settings
"landing view" cycle row) offering three values: **History**, **Browse**, and
**last watched**. `last_watched` is the shipped default (ROD-458). History is the
fallback for any unrecognized value and the degrade target for a landing that
cannot resolve, so it is the floor under all three: it is the only landing backed
by real data on launch, where a Browse landing shows its idle search prompt
(Browse is catalogue *search*, not a feed). `last_watched` opens the
most-recently-watched show's detail pane parked on its resume episode, and falls
back to History whenever there is nothing to resume (empty history, every row
never played, or a failed episode fetch; see below). Discover is not a landing
option (§11.5).

**Resume landing (`last_watched`).** Resolved once, on the *initial* history load
only, never on a mid-session reload after playback. The most-recently-watched show
is the first row with a non-null `last_watched_at` (the history load sorts those
first). The episode-grid seed positions the cursor on the resume episode (▸ marker
when an unwatched episode exists); a caught-up show opens with the cursor parked
and no marker. If the grid fetch fails (offline / provider error) the auto-open
demotes to the History view with a toast rather than stranding a blank detail
pane. Empty or never-played history simply lands on History.

**Normal state (DB has rows).** Reuse the §5.4 layout verbatim. The top bar reads
`SABIGOKU  ░  Watchlist  冬 2026` (the season chip mirrors the focused row, or the
current cour when it has no season). §8.1's degrade rules apply to any null fields
in each row (season chips and score badges are omitted; progress bars degrade
gracefully when `total_episodes` is null).

**First-run empty state.** When the DB has zero rows (a fresh install, or a user
who has never played anything) the History view cannot show a list:

```
  SABIGOKU  ░  Watchlist  冬 2026                                                 ·

                                                                                     [spacer rows]




                               nothing watched yet                                   [m + italic, centered]
                               D  see what's popular                                 [m, centered]
                                B  search for a show                                 [d, centered]




                                                                                     [spacer rows]
  ▌  D discover · B browse · q quit
```

Rendering rules:

- `nothing watched yet`: centered in the viewport (horizontal and vertical center
  of the rows between top bar and bottom bar). Color: [m] + italic. Italic marks
  absent-state annotation throughout the app, not content.
- `D  see what's popular` / `B  search for a show`: two hint rows below the
  headline, centered, mirroring Browse's own three-element absent state. The key
  glyphs (`D`, `B`) are [f] + bold to mark their role as actions. The `D`
  (Discover) line is the primary path at [m]; the `B` (Browse) line recedes to [d]
  for the users who already know the title. An empty watchlist is a user who
  doesn't yet know what to watch, so the first action is the zero-input Discover
  feed, not Browse's blank `/` prompt (§10). Bold alone marks the keybind here;
  it is the same treatment the help line uses (§1.3).
- Bottom bar: idle help line as normal (§3.5 State 1), including the `▌`.
  The empty state does not suppress navigation.
- The message block is treated as a unit for centering: headline at `mid -2`, the
  `D` hint at `mid`, the `B` hint at `mid +2` (the §8.4 spacing Browse uses), each
  horizontally centered.
- No section headers, no `─` rules, no progress bars. The screen is the void until
  the user heads to Discover (`D`) or Browse (`B`); the `/` filter is suppressed
  here (an empty watchlist has nothing to filter).

**Browse first-run absent state.** Empty Browse names itself and teaches the next
action: `search the catalogue` headline at [m] + italic, `/ find anime` primary
hint, `P save` receded secondary at [d]. Token tier rule: actionable first-run
headlines (`search the catalogue`, `nothing watched yet`) render at `text.muted`
(fg2), one step brighter than the non-actionable persistent absences (`no art
yet`, `no episodes`, `text.dim`/fg3), because they invite action rather than mark
a dead end; key glyphs are `state.focus` bold and the receded secondary hint drops
to `text.dim`. This extends the §1.3 "placeholder/hint = text.dim" rule with a
brighter tier for actionable states; no new palette entry (§10).

### 8.4 Empty Search Results

The user submitted a query and AniList returned zero media: the show does not
exist in AniList's index, or the query matched nothing.

**List column:** render `no results for "<query>"` in [m] + italic, **centered**
(matching the absent states above, not pinned to the top-left), with `try a
different spelling` one row below in [d] + italic. No list rows, no section
headers; the bottom-bar search prompt stays visible so the query is kept.

**Bottom bar (search state):**

```
  /  xyzzy_                                                      [catalogue · 0]
```

The result count `[catalogue · 0]` in [m] is already sufficient signal; the
`catalogue` scope tag also separates it from History's `[history · N]` filter
count, so network-vs-local reads at a glance. No toast is issued for zero results;
this is an expected search outcome, not an error.

**Detail pane:** clears to `color.bg` fill. No stale detail from the previous
selection remains. If nothing is selected, the detail pane is blank.

**Returning to a non-empty state:** as soon as the query changes and results
arrive, the list re-populates. No explicit "clear" action required.

### 8.5 Source Unreachable

Two distinct failure surfaces, matching the two network dependencies:

**AniList unreachable (catalog brain).** On a search attempt (search state active,
the live AniList call fails with a non-200 or network error):

1. The bottom bar remains in search state with the query visible.
2. A `[!]` error toast fires per §4.7: `can't reach AniList` in [h] + bold,
   `bg.elevated` background. This toast is **persistent** (§4.7): it does not
   auto-dismiss in the usual 2.5s; it clears when the next successful response
   arrives. (Implementation: the `persistent: bool` field on `Toast`; persistent
   toasts are only removed when explicitly cleared by the success path.)
3. The list column shows any previously cached results if available, or `no
   results` in [d] if the cache is also empty.

The Discover feed renders its own in-grid unreachable state instead (§3.8:
`[!] can't reach the feed` + `check your connection`, persistent until a refetch
succeeds).

The History view (if any rows exist in the DB) is always accessible during an
outage: `H` navigates to it normally. Local data survives a network outage.

**Provider unreachable (streams/episodes).** Episode-list and stream-resolve
failures surface as the transient `episodes_error` / `play_error` toasts (§4.10),
classed by cause (`network unreachable`, `{provider} blocked us`, `{provider} is
down`, `{provider} returned an error`). These are point-in-time failures: the
condition is already over and the user can retry, so they are not persistent.

**Recovery:** the first successful response from the failed source clears its
persistent toast and returns the UI to normal state.

### 8.6 Discover: Data Layer

Discovery is a single global catalogue concern, not a provider-relative one, so
Discover talks to AniList directly rather than through the `StreamProvider`
registry.

**Query.** `Page(page: N, perPage: 20) { pageInfo { hasNextPage } media(sort:
[...], type: ANIME, season: ..., seasonYear: ...) { ...GQL_FIELDS } }`: the same
full field set the search/by-id paths use. One call returns a fully-enriched page:
title, cover, score, genres, season, format, episodes all arrive together; no
separate enrich pass.

**Axis → query mapping:**

| Axis | `sort` | Extra filter |
|---|---|---|
| Trending | `[TRENDING_DESC, POPULARITY_DESC]` | - |
| Popular | `[POPULARITY_DESC, ID_DESC]` | - |
| Top Rated | `[SCORE_DESC, ID_DESC]` | - |
| This Season | `[POPULARITY_DESC, ID_DESC]` | `season`/`seasonYear` = current cour (same season-boundary logic as the §2.3 kanji chips / the `NEW` badge) |

Every axis carries a secondary sort key (`POPULARITY_DESC` or `ID_DESC`) purely
for pagination stability: AniList ties on the primary key at the tail of a large
result set, and an unstable tiebreak would reshuffle already-loaded pages on the
next fetch, desyncing the grid's rank numbers from what the user already scrolled
past.

**Per item:** rank position (derived from result index + 1 on the receiving
side), cover URL (`coverImage.large`, an absolute AniList URL), romaji + english +
native title, `score`, `genres`, `season`/`seasonYear`, `format`, `episodes`,
`id`/`idMal`. `TOP` and `NEW` badges stay **derived render-side**, not payload
fields. `TOP` is always rank #1. `NEW` is computed from season/year against the
current cour (§2.3 logic) and is suppressed outright on the `This Season` axis
(§3.8: every card there is already this-cour, so the badge would fire on every
card and stop meaning anything).

**Pagination:** `perPage: 20`, `page` increments on each next-page fetch, gated by
AniList's own `pageInfo.hasNextPage` (an explicit signal, not a "page came back
short" heuristic). The next page prefetches when the grid cursor comes within 2
card-rows of the last loaded entry. An axis change resets to page 1, clears
results, and refetches from the top. AniList's ~90 req/min rate limit means only
an axis-switch or page-advance cache miss spends budget; the per-axis slot cache
(below) is what keeps casual axis-flipping from burning it.

**Per-axis slot.** Each axis (`Trending` / `Popular` / `Top Rated` / `This
Season`) holds its own result list, cursor, scroll position, loading flag, and
exhaustion flag. Switching axes preserves each slot's state. An axis whose slot is
empty at activation triggers a fresh fetch. `score`/`genres`/`format`/`episodes`
are properties of the media object itself, not the axis (the same show carries the
same score and format no matter which axis surfaced it), so they may be shared and
persisted across slots. Only rank/position stays axis-relative.

Detail zoom (`Enter`) fires the episode-list fetch for the grid; that data comes
from the provider registry and is not part of the feed query.

**Null-degrade rules.** AniList's response is complete on arrival, so these are
genuine per-field data gaps (unannounced titles, an obscure entry missing a field):

| Field | Present | Absent fallback |
|---|---|---|
| Cover URL | cover art: Kitty image, or half-block mosaic on non-Kitty terminals (§3.8) | `bg.surface` placeholder fill (§3.8) |
| Format + episodes | `TV · 24ep` / `Movie` / `TV · ??ep` in `text.muted` (§3.8) | `—` in `text.dim` |
| Score | `[NN]` badge, tier-coloured per §2.2; 91+ capped at `text.primary` on cards | `[--]` in `text.dim` |
| Genres | up to 2 genre glyphs in `text.dim`, right-anchored on the format row (§3.8a) | glyph pair absent |
| Season/year | `NEW` badge derivation (suppressed on `This Season`) + top-bar season chip for the selected card | `NEW` badge suppressed; chip absent |
| Primary title (`title_language`, §8.2) | shown, clipped | never blank: the chain backstops to romaji |

---

## 9. Implementation Handoff (Rust / ratatui / crossterm)

This section maps the design system to the ratatui + crossterm stack so the build
can go straight from spec. Module paths below are the planned homes; each "owns"
entry is a single-source-of-truth contract: do not re-derive its numbers or
strings elsewhere.

| Module | Owns |
|---|---|
| `src/tui/theme.rs` | Every color token (§1.1) and the four `Palette` instances (§1.4) |
| `src/tui/layout.rs` | `pane_split(w)` and `PANE_SPLIT_MIN` (§3.2) |
| `src/tui/view/detail.rs` | `DETAIL_TWO_COL_MIN`, `cover_height_cap` / `synopsis_cap` (§3.3), `meta_line` / `provider_line` / `alt_rows` (§5.3a, §4.4) |
| `src/tui/view/discover.rs` | `GENRE_GLYPHS` (must match §3.8a exactly), card-grid geometry (§3.8) |
| `src/tui/view/browse.rs` / `src/tui/view/history.rs` | List-row rendering (§4.1), History preview stack (§5.4a) |
| `src/tui/view/settings.rs` | Settings rows; the compile-time section-boundary assertion (§5.5) |
| `src/tui/view/connect.rs` + `src/anilist/login_loopback.rs` | AniList connect modal + OAuth loopback and callback pages (§5.5a) |
| `src/tui/chrome.rs` | Top bar (§3.4) and bottom bar / help strings (§3.5, §7.5) |
| `src/tui/render.rs` | `bar_fill_color` / `bar_frac_color` (§4.5, unit-tested) and shared styling helpers |
| `src/tui/app.rs` | `AppState` (§9.6), `detail_meta_fields()`, `failure_class_copy`, effective provider preference |
| `src/anilist/` | GraphQL client, `GQL_FIELDS` (§8), OAuth |
| `src/provider/` | `StreamProvider` trait + the multiprovider registry; one module per concrete provider (`src/provider/allanime.rs` first, more following it) |
| `src/domain.rs` | `Anime` and friends; `expected_episode_count` |
| `src/store.rs` | SQLite store; `recompute_progress`, `get_provider_pin` / `set_provider_pin` |

### 9.1 Cell Styling (ratatui `Style`)

ratatui renders to a cell buffer where each cell has a symbol, fg/bg `Color`, and
a `Modifier` bitset (`BOLD` / `DIM` / `ITALIC` / `SLOW_BLINK` / …). Colors are
truecolor RGB:

```rust
use ratatui::style::{Color, Modifier, Style};

const FG:  Color = Color::Rgb(0x39, 0xff, 0x6a); // text.primary
const BG:  Color = Color::Rgb(0x02, 0x0d, 0x06); // bg.base
const HOT: Color = Color::Rgb(0xff, 0x2d, 0x78); // state.now
// ... etc for all tokens
```

Apply per-token styles via a helper that takes a semantic role and returns a
`Style`: this is the token lookup, not inline hex everywhere. Every widget draw
call composes `Style::default().fg(palette.fg).bg(palette.bg)` from the active
`Palette`'s fields (§1.4, §9.7).

Because §0 paints the void edge to edge, every root render pass fills the full
frame area with `bg.base` before drawing content: cells never keep the terminal's
own default background.

### 9.2 Pane Layout (ratatui `Rect`)

The layout is plain `Rect` arithmetic. Do **not** route the main split through a
constraint solver (`Layout` percentages): the §3.2 integer formulas are the source
of truth, and a solver's rounding could drift from them. Compute the rects
directly:

```rust
// src/tui/layout.rs
pub const PANE_SPLIT_MIN: u16 = 60;

pub struct PaneSplit { pub list_w: u16, pub detail_x: u16, pub detail_w: u16 }

pub fn pane_split(w: u16) -> PaneSplit {
    let list_w = (w * 38 / 100).max(30);
    let detail_x = 2 + list_w + 2;          // 2-cell left margin + list + 2-cell gap
    PaneSplit { list_w, detail_x, detail_w: w - detail_x - 1 }
}
```

Sub-rects per frame: top bar (1 row, full width), spacer, list column, detail
column, cover art region (child of the detail column), bottom bar (1 row, full
width). List/detail content height is `frame.height - 3` (top bar, spacer, bottom
bar). ratatui rects are content regions with no implicit borders, which is correct
for Terminal Ghost: never attach a `Block` with borders to a pane.

### 9.3 Cover Art (`ratatui-image`, spiked: ROD-417)

The pipeline, validated end to end by `spike_cover` (**normative artifact:**
sabigoku `examples/spike_cover.rs`; findings in `SPIKES.md` §6):

1. Fetch cover image bytes (JPEG/PNG) from the AniList URL via HTTP.
2. Decode to pixels (the `image` crate).
3. Build the protocol state via `ratatui-image`'s `Picker` (which queries the
   terminal for graphics support and font/cell pixel size on init).
4. Render into the cover sub-rect with the stateful image widget, cropping to
   fill the block (§3.3: crop, never letterbox).

When Kitty graphics are unavailable, `ratatui-image`'s halfblocks fallback renders
the image via `▀`/`▄` cells (§3.3).

**Spike verdicts (2026-07-17, ROD-417).** Protocol detection: ghostty lands on
Kitty with cell pixels reported (9x20 measured); tmux lands on halfblocks with no
geometry, so the fixed fallbacks (7/5-row cards, 28/20-row detail caps) engage
exactly as specced, with no escape garbage. The §3.8 adaptive height derivation
checked out to the row (20-col cover at 9x20 px -> `cover_h = 13`). No conflict
between image placement and ratatui's diff-based rendering was observed across
resize storms. Cost: ~1 ms worst draw frame, ~4 ms mean Kitty encode per cover,
off-thread, zero encode errors.

**Threading contract (landmine).** Resize+encode runs on a worker via
`ratatui-image`'s `ThreadProtocol`, and its request ids count per instance: N
images sharing one worker channel cannot route responses by trial (colliding ids
would install the wrong poster). Give each image a private request channel and
drain them into an index-tagged worker queue, as the spike does.

Cache the decoded pixel buffer; re-render at new cell dimensions on resize, never
re-fetch from network (§9.5).

### 9.4 Input Handling

crossterm delivers `Event::Key(KeyEvent { code, modifiers, .. })`. Map
`KeyCode::Char(_)`, `KeyCode::Enter`, `KeyCode::Esc`, and `KeyCode::F(1..=4)` to
the §6.1/§7.2 tables.

The `/` and `:` keys transition the app's `input_mode` state field:

```rust
enum InputMode { Normal, Search, Command }
```

In `Search` mode, printable characters append to `search_query`. On each
keystroke, recompute the filtered result set and re-render the list column (the
live AniList fetch itself is debounced; §4.10).

The `▌` carries no blink modifier: it is a steady `color.hot` cell (§4.9,
ROD-481). A bottom-bar test asserts no cell in any bar state carries
`SLOW_BLINK`/`RAPID_BLINK`.

### 9.5 Resize Handling

crossterm sends `Event::Resize(w, h)`. On receipt:
1. Recalculate the §3.2 split and every derived rect from the new dimensions.
2. Recalculate the cover art block size (§3.3 breakpoints).
3. Force a full redraw.

The cover art image must be re-rendered at the new cell dimensions on resize, from
the cached decoded pixel buffer.

### 9.6 State Machine Overview

```rust
struct AppState {
    active_view: View,              // Browse | History | Detail | Discover | Settings (§7.1)
    active_pane: Pane,              // List | Detail: pane focus within a view (§7.3)
    detail_origin: DetailOrigin,    // Browse | History | Discover: where .detail was entered from, for the Esc chain (§7.4)
    input_mode: InputMode,          // Normal | Search | Command (§3.5)
    list_cursor: usize,
    detail_scroll: usize,
    episode_cursor: Option<usize>,
    search_query: String,
    results: Vec<domain::Anime>,    // source-agnostic domain type
    selected: Option<domain::Anime>,
    cover: Option<CoverState>,      // decoded image + protocol state, once the URL is known
    loading: bool,
    sync_active: bool,              // manual metadata refresh in flight (§6.3)
    source_error: bool,             // persistent unreachable state (§8.5)
    confirm_delete: Option<usize>,  // armed hard-delete row (§6.5)
    toast_queue: Vec<Toast>,
}
```

Defaults: `active_view` seeds from the `landing` config at startup (History for
both `"history"` and the default `"last_watched"`, which then auto-opens the
resume detail, §8.3); `active_pane = List`; `detail_origin = Browse`. This maps
directly to the component state specs in §4. Each render pass reads from this state and
writes cells; no retained rendering state.

### 9.7 Color Token Constants File

`src/tui/theme.rs` holds every token. Every cell styling call references these;
never inline hex in component code. The constants below are the **Terminal Ghost**
source values; the file also wraps them (plus the `phosphor`, `nord`, and
`tokyonight` themes) in a `Palette` struct selected at runtime (§1.4).

```rust
// src/tui/theme.rs: Terminal Ghost source values
pub const BG_BASE:     Color = Color::Rgb(0x02, 0x0d, 0x06);
pub const BG_SURFACE:  Color = Color::Rgb(0x06, 0x14, 0x10);
pub const BG_ELEVATED: Color = Color::Rgb(0x0b, 0x1f, 0x18);
pub const CHROME:      Color = Color::Rgb(0x1a, 0x40, 0x30);
pub const FG:          Color = Color::Rgb(0x39, 0xff, 0x6a);
pub const FG2:         Color = Color::Rgb(0x2a, 0x60, 0x40);
pub const FG3:         Color = Color::Rgb(0x16, 0x35, 0x25);
pub const FOCUS:       Color = Color::Rgb(0x20, 0xff, 0xdd); // overdriven, §1.1
pub const HOT:         Color = Color::Rgb(0xff, 0x2d, 0x78);
pub const WARN:        Color = Color::Rgb(0xe5, 0xb8, 0x00);
```

This file is the single source of truth. Render code reads the active `Palette`'s
fields (§1.4), so tweaking a color, or switching themes, happens in exactly one
place.

### 9.8 Event Loop and Runtime

The runtime is **`std::thread` + mpsc** (ROD-431/433, §11.1, §10): a blocking
crossterm event reader thread, worker threads for network and playback, and one
mpsc channel into a tick-driven render loop. reqwest's own runtime stays
contained behind `blocking`. tokio was the rejected alternative; §10 holds the
reasoning and the trigger that would reopen it.

What the doc binds independently of that choice, and what would survive a swap:

- The **event vocabulary** of §4.10: the named in-progress flags, terminal-outcome
  events, and deliberate silences must exist regardless of runtime.
- The **spinner clock**: all in-flight operations share `async_start_ms` +
  `is_slow_path()` for the >3s `state.focus → state.now` escalation (§4.8).
- The **cover settle debounce** (150ms, §6.4) and the search debounce (§4.10).
- **Render purity**: render passes read `AppState` and write cells; workers never
  touch the terminal.
- **Startup** does no network work (§5.6): open DB, load history, land.

---

## 10. Design Decisions Log

Deliberate calls made where the brief was underspecified, plus ratified calls
carried over from the design's origin (ROD-XXX ids are provenance pointers into
that history). Logged here so they can be revisited without archaeology.

| Decision | Rationale | Revisit trigger |
|---|---|---|
| Cover art crops (no letterbox) | A cropped image reads like a cover; letterboxed reads like a viewer with empty bars. The poster aspect ratio is the content. | If key art is consistently cropped badly, add letterbox as a toggle. |
| Cover image footprint fill only: no `bg_surface` matte around the rendered image (ROD-164) | The slot geometry (fixed cell dimensions vs the poster's pixel aspect) produces unavoidable non-zero fit-matte at arbitrary terminal sizes; the cover math is rebuilt each frame from reported pixel/cell metrics that don't divide cleanly. Filling the full slot with `bg_surface` exposes this as a contrasting matte whose size varies with terminal geometry, and `bg_surface` means "elevated layer" (§1.1); mounting hero content in it is a semantic collision. Instead only the image footprint is painted; the leftover slot inherits `bg_base`, so the mismatch has nothing to contrast against (§0 "panes float in the void", §3.3 "no border"). `bg_surface` is preserved for placeholder states (loading spinner, "no art yet") where the panel itself is the content. PNG alpha composites onto `bg_base`. | If covers with heavy alpha transparency look wrong on `bg_base`, add a `bg_surface` fill scoped to the fit rect only (not the full slot). |
| Single magenta cursor, not per-pane focus indicators | Two simultaneous magenta elements dilute the "pointer" semantic. The `·` dot in the top bar handles pane focus in `state.focus` (cyan) only. | If pane focus proves unclear, move the active pane label to a more prominent position. |
| No animation on state transitions | Terminal Ghost's identity is restraint. Since the cursor blink was retired (ROD-481) nothing claims the temporal channel at all, and a transition is a poor candidate to be the first thing that does. | If feedback identifies a specific transition that needs clarification, add a single-frame flash (not a slide). |
| Transparent mode resets only the `bg` tier; `surface`/`elevated` stay painted (ROD-511) | The base tier is the whole canvas (list and detail pane alike, ROD-458 F4), which is exactly what the user opted to see through; focused rows, cards, and toasts keep §1.1 contrast as opaque islands. No app-side alpha: the backdrop is unreadable and no opacity query exists, so `Color::Reset` is the only hook (§1.4a). | If painted islands over a blurred backdrop read as floating patches rather than layers, consider resetting `surface` too and carrying depth on borders and fg tiers alone. |
| Kanji season/status chips without box borders | Box around kanji chips adds visual noise against an already dense detail pane. Color alone is sufficient on dark. | If user testing shows the chips are missed, add a dim `[` `]` wrap in `border.hair` color. |
| Help line updates contextually per view | The bottom bar doubles as a contextual hint line. Fewer permanent labels means less to ignore. | If users report confusion about available keys, add a `?` keybind that shows a full key reference in `bg.elevated` overlay. |
| Score ≥ 91 earns `state.now` | The 91 threshold maps to AniList's "Favorites" tier. Below 91, scores are metadata. Above, they are a claim. | Adjust threshold if the distribution feels wrong in practice. |
| List column 38% / detail 62% at default width | Tested against 120-col and 160-col terminals. 38% gives ~45 chars for the list: enough for most anime titles without truncation. Detail gets the rest. | Adjust if common terminal widths expose truncation problems. |
| `state.focus` (cyan) gated to the selected, list-focused row; status colors step off it (ROD-194) | One token can't mean both "the cursor" and "this show is airing/watching": a fully-filled watching bar in `state.focus` out-shouts the selected row, and pane focus becomes invisible when the selection looks identical focused or not. Reserving cyan for `selected and list_focused` fixes both: unselected watching/paused step down to `text.muted` (the `▸`/`◐` glyph still carries the status), and losing list focus visibly recedes the selected row (band drops, `▸` dims, title un-bolds). The `·` dot stays as low-weight orientation; the list itself carries the focus signal. Magenta remains reserved for the status-bar cursor. | If watchlist scanning suffers because watching rows no longer read as a cyan "heat signature" at a glance, trial `state.focus` dim (not full) for unselected watching, or widen the `text.muted`↔`text.dim` gap so watching vs completed bars stay distinct. |
| `r` (not `:reset`) for progress recompute in History (ROD-193) | Single-level undo (`u`) goes stale after any subsequent key; the recovery window is one action, so the recompute deserves a direct key rather than waiting on `:` command mode. `r` is non-adjacent to `c` on Colemak-DH, so it can't be mis-keyed in the same motion. Recompute uses the sorted-index, translation-scoped strategy: `progress` = 1-based ordinal of the last fully-watched row among the `episode_progress` rows present, sorted by episode sort key. Intentionally under-counts gap-watching (only rows that were started are present). `Store::recompute_progress` is the single source of truth for these semantics. | If users want a "reset to 0" shortcut independent of the strategy, note that a show with no fully-watched rows already recomputes to 0; suggest deleting episode_progress rows via a future `:clear progress` command. |
| Genre glyphs stay `text.dim` (`fg3`), single-space separated (ROD-247) | The glyphs are ambient texture, not a label; `text.dim` is the register for "present but not asking to be read." The real legibility problem was the two glyphs smushing into one shape, not brightness: a single space between them fixed that. `text.muted` (`fg2`) was tried and reverted: brighter made the glyphs compete with the format and episode count for attention, which own that row (`TV · 24ep`, §3.8). The spatial split (count left-anchored, glyphs right-anchored at the cover edge) plus the dim tier keeps each in its lane. Legibility ratified at `fg3` (ROD-509, §11.4): the confusable pairs separate by fill, which dimming preserves, so the separator space is the mechanism holding them apart and every glyph added to §3.8a inherits it. | If the glyphs prove invisible in practice, widen the inter-glyph gap or drop to one before re-dimming. |
| Nord `focus` stays hue-shift, **not** a luminance lift (ROD-184) | Nord violates the §1.1 focus-clears-`fg`-luminance rule: `focus` (nord8 frost, L 0.475) reads dimmer than `fg` (nord4 snow, L 0.727). The alternative was overdriving `focus` to a snow-storm value (nord6, L≈0.83) so the rule stays universal. Rejected: Nord's `fg` already sits the brightest of the four dark palettes, and lifting `focus` past it would push Nord toward a light-theme read, fighting §0's dark-only constraint. The nord8 hue-shift + bold carries focus distinction without adding luminance, faithful to Nord's own palette relationships. So the focus-clears-`fg`-luminance rule stays non-universal (§1.4): Terminal Ghost / Phosphor / TokyoNight honour it; Nord trades it for hue. A deliberate call, not a deferred fix. | If testing shows the Nord focused row is genuinely hard to locate (hue-shift + bold insufficient), revisit the lift, but bound it so Nord's `focus` does not cross into light-theme luminance. |
| Studios collapse-format caps at 2 named studios, `+N` beyond (ROD-261) | Mirrors the §3.8a genre-glyph cap (≤2, ambient rather than exhaustive) and keeps ~~the rail's 8-col gutter from being blown out by~~ the compact line's width budget safe from a multi-studio co-production credit list. The full list still lives in the persisted column for any future full-detail surface; ~~this only caps the rail's render~~ this only caps the compact line's render (ROD-458). | If a 3-studio credit is the common case rather than the outlier, raise the cap to 3 before reaching for a wrap. |
| Rank prefers a contextual RATED ranking over POPULAR when both exist, and drops the season name even for a season-scoped rank (ROD-261) | RATED reads closer to Rank's quality-signal intent: raw popularity was rejected as noise (§10.1), and a contextual *rank* needs to read as a different, sharper signal than that rejected field, not a rebrand of it. The season name is dropped from the render even when AniList scoped the ranking to a season, because the header's own season/year chip (§4.4) already carries that context on the same screen; repeating it would waste space Rank's 8-col gutter doesn't have. | If a season-scoped rank without the season name reads ambiguous in testing (e.g. a show that spans two cours), reconsider a compact season glyph. |
| Airing countdown collapses to one coarsest unit and omits itself once stale, rather than showing a negative/zero value (ROD-261) | A combined `Nd Nh` value doesn't fit the chips row's terse register, and a countdown that has silently lapsed (a stale `nextAiringEpisode` in the window between the real airing time and the next metadata refresh) would read as a bug if shown as `-2h` or `0d`. Omitting it instead degrades to the same "no countdown" state a show without airing data already renders: a known-good degrade (§8.1), not a new one. | If users want confirmation an episode aired without waiting for refresh, consider a distinct "just aired" state instead of silent omission. |
| Non-JP origin marker is a bare two-letter country code in `text.dim`, trailing last on the chips row, not a flag glyph (ROD-261) | An actual flag emoji is a Supplementary-Plane regional-indicator pair, outside the §2 "glyphs must fall inside the BMP" contract, and wouldn't render deterministically across this app's terminal targets. The dimmest available tier plus last-in-order placement keeps a rare, static fact from competing with the row's live status/season/countdown information; the common JP case shows nothing. | If CN/KR-origin shows are common enough in a user's library that the marker starts carrying real signal rather than being incidental, promote it to `text.muted` or a small dedicated icon. |
| Italic stays pinned to the native-language title field, not to "whichever row is currently an alt" (ROD-205, §1.3/§8.2) | Generalizing italic to "any non-primary title row" would make the English alt row render italic under the default `romaji` preference, a visual change with no benefit. Keeping italic keyed to the native/Japanese-script field specifically preserves italic's §1.3 meaning (foreign script, not row position) while generalizing correctly for all three preferences: native gets italic whenever it lands in an alt slot and loses the treatment once it becomes primary (every primary line is bold, never italic). | If an English alt row reads too flat next to an italic native row in practice, reconsider as a fresh value judgment. |
| ~~Rail's~~ ~~Pinned field~~ The provider row shows the raw provider name, not `display_name()` (ROD-345; ROD-484 moved the rule from the pin's own segment onto the tokens, which already followed it) | Matches the Settings `provider` row (§5.5), which renders the raw stored preference string. It is the same persisted identifier, just scoped per-show instead of globally, so consistency with that precedent outranks matching the toast-prose convention (`{provider}` = `display_name()` everywhere a sentence names a provider, §4.10). The split is deliberate: config-surface rows echo the stored identifier; user-facing prose speaks the display name. | If a provider's stored name and display name diverge enough to confuse users ~~in the rail~~ on `provider_line`, reconsider as a ~~rail-specific~~ `provider_line`-specific formatting fix, not by changing the Settings row's convention. |
| Provider field folds availability and serving into one ~~rail~~ row, ASCII markers (`▸ + - ?`) instead of a new pictographic set (ROD-348/356) | A separate serving row plus a separate availability row would put three provider-ish rows next to each other (Pinned already there) for information that is really one axis per provider (what's known, what's active): one row reads as one fact family. The `◆`/`◈` and `◐`/`◎` glyph pairs carried an open dim-legibility question at the time (since resolved in their favour, §11.4); rather than risk a fourth confusable pair, the tri-state (plus "serving") uses plain ASCII shapes and reuses the already-proven `▸` resume glyph for "serving", a genuinely different glyph family. Shape carries the state rather than color because the Phosphor theme (§1.4) is monochrome, so a color-only distinction would be illegible there. | The dim-legibility question resolved in the pairs' favour and the ASCII set still stands on its own reasons (font-independence, zero substitution risk); a future marker set does not get to cite the pairs' legibility as licence to go pictographic here. |
| Provider field lists providers in fixed registry (construction) order, not the per-walk preference order (ROD-348) | The preference order is a resolve-order hint, recomputed per preference and per walk; using it here would make the same show's ~~rail~~ line read in a different order across sessions as the user's global or per-show preference changes, breaking "scan the same column, same order, every show." ~~The rail is~~ This field is a status display of what's out there, not a queue of what to try next, so it wants a stable reference order. | If the registry grows past 4-5 providers and scanning a long fixed-order row gets noisy, consider grouping bound-first rather than reordering by preference: preference and "what's out there" are still different questions. |
| Provider and Pinned surface on their own dedicated row, not as segments of the joined `·` meta line (ROD-348/356) | ~~A `rail_only` field never blooms below `DETAIL_TWO_COL_MIN`, which would leave Provider and Pinned invisible on every compact-width detail pane, exactly the width most terminals run at day to day.~~ Folding them into the joined line itself was rejected: routing/session state is a different category of fact from the AniList metadata the joined line otherwise carries, and interleaving muddies both. ~~The fix lives entirely in a bespoke compact-form row drawn beneath the joined line (`draw_provider_line`), outside the generic field-list iteration either renderer uses.~~ The fix lives entirely in `provider_line`, a dedicated row atop the episode grid, outside the generic field-list iteration `meta_line` uses (ROD-458: this row is now unconditional, not a compact-width special case). | If a third field ever needs the same "own row" treatment, generalize `provider_line` into a small family of bespoke rows rather than routing a third concept through it by special case. |
| The pin's UI representation is retired outright; a probing token marks an in-flight walk hop instead (ROD-348/356 → ROD-484 → ROD-524 → **ROD-525**) | Four rounds on the same problem, each fixing the cost the last one paid: ROD-348/356 gave Pinned its own row with a `pin ` prefix segment; ROD-484 replaced the text with a bold boost on Provider's own token, gated on a confirmed `▸`/`+` marker; ROD-524 ungated that boost once the settle window made pin-on-unconfirmed the ordinary state, and an invisible pin started reading as a snap-back to the serving provider. ROD-525 removes the pin itself (03 §5.1: sticky last-used replaces the two-tier preference), so there is no longer a persistent per-show choice to represent. The bold register now marks whichever token an active walk is currently probing, live-session state with nothing stored (§5.3a); it clears the instant the hop lands. | If a per-show preference concept returns (03 §5.1's deferred language-intent epic), it needs its own static marker decided fresh, not a resurrection of this chain's bold register: that register now means "in-flight," not "chosen." |
| `X` is History's one destructive key with **no undo path**, so it gets an armed-confirm layer with a separate confirm key (ROD-220) | Every other History key, including the uppercase `P` ("plan it") and the view switches, is additive or navigational: reversible by another keypress or covered by `u`'s single-level undo (§6.1). Hard-delete cascades the DB row and its episode history; there is nothing for `u` to restore. That is a step change in severity, so it gets its own confirmation layer (§4.2, §6.5) instead of the toast-and-undo pattern the rest of History relies on. Splitting execution onto a separate `y`/`Y` key rather than "press `X` twice" specifically defeats key-repeat: a held or auto-repeating `X` keeps delivering `X` (a no-op once armed, §6.5), never `y`, so a repeat storm cannot self-confirm the delete. | If a second no-undo destructive action is ever added, reuse this pattern (armed state plus distinct confirm key) rather than inventing a fresh one. |
| Bold, not underline, is the keybind-hint treatment everywhere (ROD-220) | Underline was the original spec for hint keys and was retired: it would be a one-off treatment nothing else uses, while bold-as-promotion (§1.3) already carries the identical role in the confirm prompt, the help lines, and the top-bar strip. One treatment, everywhere. | No revisit expected. |
| Any non-`y` key cancels the armed confirm; it does not absorb-and-stay-armed (ROD-220) | The forgiving reading: a stray keypress (typo, accidental arrow) drops back to idle rather than trapping the user in a frozen bottom bar they didn't mean to enter, at the cost of a re-press of `X` to retry. `X` itself is the one carve-out (no-op, stays armed, §6.5); that exception exists purely to block key-repeat self-confirm, not to generalize into a broader absorb list. | If testing shows accidental cancels are common, reconsider a narrow allowlist of truly inert keys before reopening "absorb" more broadly. |
| Cover block renders "no art yet" (persistent absent), not a spinner | A spinner implies a fetch is in flight. When there is no cover URL to fetch, a spinner would be a lie. The absent state must be visually distinct from loading. | The block keys off the URL: spinner then image when a URL is known, "no art yet" only when the URL stays null. No code change needed at the cover block. |
| Score placeholder `[--/100]` (detail) / `[--]` (list) in [d] rather than omitting the score field | Preserving the score reservation keeps column alignment stable whether or not a score is present. A missing field would shift the surrounding layout when scores arrive. | If the placeholder is visually noisy across a full list of null scores, omit it and accept the reflow. |
| Kanji chips fully omitted when null (not a placeholder) | An empty chip `[ ]` or a dim `放映中?` is worse than nothing. The chip's meaning is the kanji; without data it is just noise. The detail header still reads clearly without it. | The omission is the per-anime fallback for shows with no status/season data. |
| No watchlist status glyph on Browse / search-result rows | History rows are loaded **from** the local store, so their watch-state is already in hand: that is why History ships status chips (§5.4). Browse results come from AniList over the network and carry no watch-state; a glyph there would mean a per-row local-DB (or cache) lookup the search path doesn't otherwise do. Adding that to the fast search path for a glyph isn't a trade Terminal Ghost makes. | If watch-state is ever cheap to have at search time (results joined against the store in one pass, or membership held in an in-memory cache) the glyph becomes nearly free; revisit then. |
| The landing is configurable; `last_watched` ships as the default and History is the floor under every landing (ROD-228/229, ROD-458) | A Browse landing has no auto-populated content: Browse is catalogue *search*, so it lands on its idle search prompt. That leaves History as the fallback for an unrecognized value and as the degrade for `last_watched`, which opens the most-recently-watched show on its resume episode and drops to History whenever there is nothing to resume. `last_watched` is the shipped default because the common session starts by continuing something, and its worst case is the History landing anyway. | If the resume auto-open proves noisy on cold boots (a failed grid fetch toasting on every launch), flip the default back to `history`; the config value is the only thing that changes. |
| Persistent source-error toast (not auto-dismiss) | A 2.5s toast for "network is gone" is misleading: it disappears and the user thinks the problem resolved. A persistent toast with a bottom-bar state change reflects the ongoing condition. | The recovery path (first successful response) clears it automatically, so there is no manual-dismiss burden. |
| Startup loading screen skipped under ~200ms | A flash of a loading screen for a DB that opens in 50ms is worse than nothing: it reads as a glitch. The threshold is a design-level call, not a perf target. | Tune if the DB open is consistently slower or faster on target hardware. |
| `PANE_SPLIT_MIN = 60` is the single detail-surface threshold; no mid-tier zoom gate (ROD-170 → ROD-259) | 60 is the minimum useful list + detail column pair (`detail_w ≈ 25`), so users get the persistent preview on common 80-col terminals. The in-pane grid renders at every two-pane width: a mid-tier gate that withheld it (forcing `Enter`/`Space` to drill into the zoom just to reach the grid at 60–99 cols) was tried and retired as a dead step. `Enter` from a focused detail pane plays; `Space` promotes to the roomier zoom. | If the preview stack is too cramped at 60–79 cols, raise `PANE_SPLIT_MIN` to 80, but test first; the goal is a useful preview, not a perfect one. A new mid-tier gate would need a new reason. |
| `DETAIL_TWO_COL_MIN` keyed to detail-pane width, not terminal width (ROD-258) | With the list co-visible the detail pane is only `term - list` (~58 cols at term 100): a terminal-width gate force-split that pane into a ~22-col cover column that clipped the meta line. Gating on the pane width the columns are actually carved from means the History persistent two-pane needs its `detail_w` to clear 100 (`term ≥ 168`), while the full-screen zoom's pane is `body_w = term - 2`, so its threshold is `term ≥ 102`. One constant, two surfaces. | If `term ≥ 168` proves too conservative for the persistent split in practice (most 100–167-col users stay single-column), consider a lower threshold dedicated to that surface instead of sharing the zoom's gate. |
| First-run absent states teach the next action, not just name the void (ROD-211/254) | Empty screens that only name the void (`nothing here yet`) or advertise a `/` whose meaning differs per view (catalogue-search in Browse, local filter in History) confuse a first run. Browse names itself and teaches `/ find anime` + `P save`; an empty watchlist points first to **Discover** (an empty watchlist is a user who doesn't yet know what to watch: Discover's job), with Browse as the receded secondary. Active search/filter counts carry a `[catalogue · N]` / `[history · N]` scope tag so network-vs-local reads at a glance (`[history · N]` ratified over the source's ambiguous `[watchlist · N]` variant). Token tier: actionable first-run headlines render at `text.muted`, one step brighter than non-actionable persistent absences (`text.dim`), because they invite action rather than mark a dead end; key glyphs are `state.focus` bold, receded secondary hints drop to `text.dim`. | If first-run users still stall, consider a one-time overlay; do not add permanent chrome. |
| `title_language` has no separate "Auto" value: only `romaji` / `english` / `native` (§8.2, ROD-205) | Every option already resolves as "preferred-with-fallback": `english` falls back through romaji then native, so it already behaves exactly like an Auto/English-preferred choice would. A fourth value would be a rename of an existing option: added config surface, no added capability. | If a genuinely different behavior is ever wanted (e.g. preferring the OS locale's script over a fixed preference), that is a new axis, not a fourth value on this one. |
| F-keys are aliases, not primary binds | The letters are the primary, memorable binds. Adding F-keys as separate primary binds would create two authoritative tables to keep in sync. Aliases give discoverability without forking the semantic. | If a future change removes the letters (unlikely), promote F-keys to primary. |
| View keys surface as a persistent top-bar tab strip; F-keys are the quiet fallback (ROD-249/250) | Putting F-keys in the help line optimized for newcomers mashing function keys, but hid the more-memorable letters: an inverted discoverability hierarchy. The four view-switch letters are symmetric (`B`/`H`/`D`/`S`, no toggle), surfaced in one canonical home (the §3.4 strip), which also freed the bottom bar (the Discover help line fits its budget). | If newcomers miss the F-keys, add them to a `?` help overlay rather than to the always-on line. |
| `·` stays lit at `color.focus` in single-pane views (Settings, Discover) | Dimming or hiding the `·` in single-pane views would make the top bar layout feel different per view: a width/position shift that reads as instability. A stable `·` at a fixed position is less interesting to notice, which is the goal. | No revisit expected. |
| `·` is dim for Browse/History list, lit for Browse/History detail (ROD-170) | The `·` follows one logic in both two-pane views: dim on list (default, no secondary selection), lit cyan on detail (user has gone deeper). Color is always cyan; magenta is reserved for the status-bar cursor. | If the dim state is missed as a focus indicator, invert: lit on list, brighter on detail. |
| Esc does not quit from Browse | Matches vim idiom and prevents accidental quit. `q` is the quit key throughout; Esc is "one level back." In Browse list normal with no modal open, there is no level back, so Esc is a no-op rather than a quit trigger. | If user feedback consistently expects Esc-to-quit, add a "press Esc again to quit" two-step. |
| `active_view` and `active_pane` are two fields, not one collapsed `mode` enum (ROD-72/180) | View identity and pane focus are independent dimensions: `.detail` is both an `active_pane` value within Browse/History and a standalone `active_view` (the zoom), and collapsing them into one enum forces artificial states. The two-field model proved the right shape. | No revisit expected. |
| Season chip is an add-on in `text.muted` beside the view strip, not a replacement in `color.focus` (ROD-186) | Two chips side by side, both cyan, would blur into one blob (§2.3: chips are distinguished by color alone, no boxes). Demoting the season chip to `text.muted` makes them distinct with zero extra glyphs, matches how season/year reads in History rows (§5.4), and leaves `color.focus` meaning one thing on the left (view identity) while the cyan `·` owns the right edge. Content rule: selected show's season+year, falling back to the current cour from the system clock, except the detail zoom, which is committed to one show and shows only its season (no fallback). Rejected: a `░`/`·` separator between two cyan chips (adds chrome, §0). | If the muted season chip is missed, brighten it one step (`text.muted` → `text.primary`) before reaching for `color.focus`. |
| One navigation grammar, two zoom levels: the persistent detail pane is the default, the full-screen zoom is opt-in (ROD-170) | Two use cases are both real: *triage scrub* (persistent two-pane preview: list stays put, title/meta/cover update on cursor move; episodes load on detail-pane entry, not hover) and *committed engagement* (full-screen zoom: detail gets the whole canvas + denser episode grid). The density argument: at 120 cols the persistent pane gives ~8 grid columns (adequate for 12–26 ep titles); full-screen gives ~14 (a real gain for long-runners). The zoom earns its keep for dense content without inflicting it on everyone. Browse and History share the same pane grammar, the same zoom key, and the same Esc-demote semantics; `detail_origin` records the entry point. | Revisit if the episode grid in the persistent pane turns out to be sufficient for all practical content (would argue for removing the zoom as unnecessary complexity). |
| `Space` as zoom toggle (promote + demote) (ROD-170) | Enter already plays episodes from the detail pane, so Enter-to-zoom would collide with Enter-to-play. `Space` is unused in Browse/History (Settings-only as a toggle), and "spacebar previews/expands" is a familiar idiom. A symmetric toggle (same key promotes and demotes) is more learnable than an asymmetric promote-only with Esc-only demote. `Esc` still demotes as the canonical "back" key. Rejected alternatives: `z` (vim `zt`/`zb` ambiguity), `o` (less obvious), `Tab` (reserved for future pane cycling). | If `Space` collides with a future keybind, `z` is the next candidate. |
| Zoom Esc demotes to `.detail` pane, not `.list` (ROD-170) | The user arrived at zoom via `Space` from `active_pane = .detail`. Esc undoes one step; demoting to the detail pane is the precise inverse, and jumping straight to `.list` would skip a level, jarring on a long episode list the user was navigating. `Space`/`h` demote identically. (Exception: at `w < 60` there is no pane to land on, so they demote to the single-column list.) `q` is not a back key; it quits. | No revisit expected. |
| The grid is always reachable: `Enter` drills toward wherever the grid is visible (ROD-170/259) | Play and zoom must be tied to where the grid is actually visible, or narrow terminals dead-end (no detail surface reachable) and mid widths can fire play against episodes the user can't see. Corrected model: the grid lives in the in-pane view (`w ≥ 60`) or the full-screen zoom (any width); `Enter` "drills toward the grid, then plays": `<60` list opens the zoom, `≥60` pane plays, zoom plays; `Space` opens the zoom from any detail context (and from the `<60` list directly). Episodes fetch on detail-pane entry at any two-pane width, so the zoom's grid is always ready once the pane has been entered. Demote is width-aware: back to the pane (`w ≥ 60`) or the list (`w < 60`). | No revisit expected; a future mid-tier gate would need a new reason. |
| Runtime is `std::thread` + mpsc, not tokio (ROD-431/433; §11.1) | The 04 worker model is thread-and-channel shaped, every M0 spike ran on blocking reqwest plus threads, and `ratatui-image`'s `ThreadProtocol` works on plain mpsc; reqwest's own runtime stays contained behind `blocking`. §9.8's bindings (event vocabulary, spinner clock, debounces, render purity) are runtime-agnostic and survive a swap. | If a feature needs concurrency the thread model can't express (cancellation trees, thousands of in-flight requests), reopen it as a fresh call rather than smuggling an executor in beside the threads. |
| Kitty-graphics covers must render in ghostty, kitty and wezterm; every other terminal degrades to halfblocks (ROD-417; §11.2) | ghostty is the daily driver and is visually ratified; kitty and wezterm ride the same `ratatui-image` protocol branch, so they are spot-checks of one implementation, not three. tmux gets halfblocks plus fixed fallback heights, which is what makes headless and self-driven runs (the capture rig, the drive-tui skill) work at all. | If a fourth terminal becomes a daily driver, ratify it visually before adding it to the matrix: sharing the protocol branch is what makes a spot-check sufficient, and a terminal outside it is a new claim. |
| The `▌` is steady, never SGR blink (ROD-481; §11.6) | ghostty never rendered SGR blink and kitty did, so one spec produced two different cursors; where it did render, a blinking block in the corner read as a stray terminal cursor rather than a status marker. Nothing else in the UI uses the temporal channel, so the retirement costs no signal and needs no fallback timer (§6.4's no-manual-timing rule holds). | A future state wanting the temporal channel has to argue for it from scratch: the bottom-bar test forbidding `SLOW_BLINK`/`RAPID_BLINK` on every cell is the gate it must move. |
| History row 1 is title-only; the episode count lives on the row-2 bar and is never duplicated into row 1 (ROD-509; §11.3) | The row pair gives each line one job: row 1 identifies (status glyph + title, and the title gets the full width before truncation), row 2 quantifies (bar + `N / M eps`). Duplicating the count into row 1 spends title width on a fact the line directly below already carries. The §5.4 comp's richer right-meta (resume indicator + season + status chips) was authored but never ratified and never built. | If finding the next episode to resume takes a scan rather than a glance (users asking where the resume number went, or reaching for the detail pane to answer it), spec a row-1 resume indicator on its own terms; reviving the §5.4 comp wholesale would drag the duplicated count back with it. |
| Toast copy uses `·` as its clause separator | Port-time call: toast copy like `mpv not found · install mpv` uses the app's own metadata-separator idiom rather than a dash. Keeps every §4.10 string one visual grammar with the rest of the chrome (this doc also bans dash separators in prose). | If a toast ever needs a true sentence break, reword the copy; do not introduce a second separator style. |

### 10.1 Metadata Field Survey (ratified ship/skip verdicts)

The survey behind the §5.3a field list: what AniList's `Media` type offers beyond
the basics, ruled signal vs. noise for a terminal watchlist. Verdict, one line
each:

| Field | Verdict | Lands | Why |
|---|---|---|---|
| `source` | Ship | ~~Rail~~ compact line: Source | Cheap, unambiguous signal; no formatting risk |
| `duration` | Ship | ~~Rail~~ compact line: Duration | Per-episode runtime is genuinely useful on a watchlist (confirmed must-have) |
| `studios{nodes{name}}` | Ship | ~~Rail~~ compact line: Studios | Cheap to fetch; needs its own persisted column |
| `rankings{…}` | Ship~~, rail-only~~ | ~~Rail~~ compact line: Rank (last, sheds first, ROD-458) | Verbose, but a contextual rank is a sharper signal than a raw count (contrast the rejected `popularity` row) |
| `nextAiringEpisode{…}` | Ship | Chips row, 3rd segment | Live and clock-relative: doesn't fit ~~the rail's~~ the compact line's static-snapshot model (§4.4) |
| `countryOfOrigin` | Ship, low-noise | Chips row, trailing marker | Non-JP-only surfacing (donghua/aeni); JP is the default and shows nothing (§4.4) |
| `popularity` | Skip | - | A bare user count; Rank already conveys standing, better |
| `tags` | Skip | - | Dozens per show, often spoiler-laden; genres already categorize |
| `trailer` / `externalLinks` / `hashtag` / `siteUrl` | Skip | - | Not terminal-actionable without a browser handoff |
| `isAdult` | Skip | - | A future *filter* input, not a ~~rail~~ metadata-line fact |
| `relations` | Defer | - | A connection graph needs its own UI |

Every "Ship" row still obeys the §8.1 no-empty rule: a field with no value emits
nothing; no placeholder, no orphan separator~~, no bare rail row~~.

---

## 11. Open Questions

Genuinely undecided items. The spec above does not resolve these; do not treat
any of them as settled by implication.

**Nothing is open as of 2026-07-31 (ROD-509).** All six entries are resolved and
kept struck in place: the resolution and its date are the record. Anything whose
resolution constrains future work also has a row in §10, with its trigger.

1. ~~**Async runtime: tokio vs `std::thread` + mpsc.**~~ **Resolved 2026-07-17
   with the M1 cut (ROD-431/433): `std::thread` + mpsc.** The 04 worker model is
   thread-and-channel shaped, all six spikes ran on blocking reqwest + threads,
   and `ratatui-image`'s `ThreadProtocol` works on plain mpsc; reqwest's internal
   runtime stays contained behind `blocking`. §9.8's bindings (event vocabulary,
   spinner clock, debounces, render purity) were runtime-agnostic and stand.
2. ~~**Cover art pipeline is unspiked.**~~ **Resolved 2026-07-17 (spike_cover,
   ROD-417): validated end to end.** Protocol detection, cell-pixel geometry and
   the adaptive heights derived from it, crop-to-fill, halfblock degrade, tmux
   survival, and off-thread encode all verified; results in §9.3 and SPIKES.md
   §6. **Support matrix (ratified):** the Kitty-graphics path must render in
   ghostty (daily driver), kitty, and wezterm; every other terminal degrades to
   halfblocks. ghostty is visually ratified; kitty and wezterm remain
   spot-checks (same `ratatui-image` protocol branch). tmux survival is verified
   (halfblocks + fixed fallback heights, required for headless/self-driven use).
3. ~~**History row-1 right-meta.**~~ **Resolved 2026-07-31 (ROD-509): not built.
   The ratified baseline is what shipped.** `draw_title_row` renders the status
   glyph and the title, nothing else; the count rides the row-2 bar as
   `N / M eps` (`src/tui/view/history.rs`). The §5.4 mock's
   `[▸12] 冬 2024 放映中` column stays in the doc as a comp of an unbuilt target,
   not as a spec. §10 carries the invariant it settles.
4. ~~**Dim-glyph legibility.**~~ **Resolved 2026-07-31 (ROD-509): the pairs hold
   at `text.dim`.** Rendered at `fg3` on `bg.base` under the README capture rig's
   terminal conditions (kitty, font 16, stock font config), `◆`/`◈` and `◐`/`◎`
   stay distinct: each pair separates by fill (solid vs outline, half vs ring),
   which survives dimming, rather than by fine detail, which would not. `❖`
   (Slice of Life), the third diamond-family entry, was checked against both and
   separates by mass: it renders visibly smaller and pinched next to `◆`'s solid
   body and `◈`'s nested outline. `⚜`/`✿` likewise. The glyphs collapse only when
   set adjacent with no separator, which is the ROD-247 finding the shipped
   renderer already obeys (`glyphs.join(" ")` at `palette.fg3`). §3.8a's
   vocabulary stands; no substitutes.
5. ~~**Discover as a landing-cycle option.**~~ **Resolved 2026-07-31 (ROD-509):
   not adopted.** The shipped cycle is `history · browse · last_watched`
   (`LANDING_PRESETS`, `src/tui/view/settings.rs`). A landing option earns its
   config slot by being somewhere a user wants to *start* every session; Discover
   is where they go when the watchlist has nothing for them, and the empty-history
   state already routes there with a single `D` (§8.3).
6. ~~**Blink support variance.**~~ **Resolved 2026-07-23 (ROD-481): blink retired
   everywhere, the `▌` is steady `state.now`.** The degrade question is moot
   because the blink is gone on terminals that honour `SLOW_BLINK` too: ghostty
   never rendered it and kitty did, so one spec produced two different cursors,
   and where it did render, a blinking block in the corner read as a stray
   terminal cursor rather than a status marker. No manual timer, so §6.4 stands
   unchanged.

## Changelog

**2026-08-01 (ROD-525):** The two-tier provider preference (global
`preferred_provider` + per-show pin) is retired. In its place: sticky
last-used, the provider each show last **landed** on, written only at a
landing (a confirmation write, never speculative) and only when it differs
from the walk-order head; opens start there and walk on failure (03
§4.1/§5.1). `v` is now a manual walk, not a pin cycle: it advances to the next
provider in registry order and tries it live, walks on automatically past a
miss, and dead-ends only when the full circle exhausts (03 §5.2; 05 §10.5).
The route-stamp mechanism and the K-2 forced-preferred law it carried are
deleted outright, not replaced: both existed to protect a speculative write,
and last-used's confirmation-write timing removes the failure mode they
guarded against (03 §5.3). §5.3a's bold pin register is replaced by a probing
token marking an in-flight walk hop, cleared the instant it lands; §10's
ROD-348/356 → ROD-484 → ROD-524 row stack on the pin collapses into one row
citing the lineage. Migration seeds `provider_last_used` from `provider_pin`
rows gated on a matching `provider_binding`, then drops `provider_pin` and
`provider_route` (02 §3.2). Verification also found `resolve::open_history`
(03 §4.1, the old "Path 2") was never wired: History opens have always run
the canonical-open path, doc/code drift dating to the original port design,
recorded rather than silently erased. Silent provider migration is accepted:
nothing re-probes an original a show has migrated away from except another
manual `v`.

**2026-08-01 (ROD-524):** `v` cycles the pin inside a 300ms settle window: each
press moves the pin in memory and re-arms, the store write and at most one flip
fire when the burst stops, and a settle needing no walk supersedes a flip still
in flight. The §5.3a pin boost lost its confirmed-marker gate: the bold token is
the selection wherever it sits, because the window makes pin-on-unconfirmed the
ordinary state and a gated (invisible) selection read as snapping back to the
serving provider.

**2026-07-31 (ROD-509):** Retitled from `Status: spec` to a living spec: the app
is shipped, the doc describes it, and code/doc disagreement is drift with a bug
on one side. zigoku's retirement is recorded in the header, so parity is no
longer a reason for anything here. §11's four remaining questions are struck in
place with their verdicts (History row 1 stays title-only; the `◆`/`◈` and
`◐`/`◎` pairs are legible at `fg3`; Discover is not a landing option; the blink
went in ROD-481), leaving nothing open. §10 gained the runtime call, the cover
support matrix, the steady `▌` and History's row-1 invariant, each with a
trigger. Corrected across the doc: every reference to a blink the code has a
test forbidding (§5.1, §5.2, §6.4, §6.5, §7.5, §5.5, §8.3, §9.4), the §8.3
landing default (`last_watched` ships as default, History is the floor under all
three, §7.1/§5.5/§9.6 with it), and §9.8, which no longer presents the runtime as
two candidate shapes.

**2026-07-31 (ROD-511):** Added §1.4a: the `transparent_background` config
toggle maps the `bg` tier to `Color::Reset` (terminal default) so the
terminal's own opacity/blur shows through; `surface`/`elevated` stay painted
as opaque islands. New `transparent background` toggle row after `palette`
(Interface, §5.5; fifteen interactive rows). Also struck the §3.1
color-differentiation clause that ROD-458 F4 had already retired in code: the
detail pane fills `bg.base`, not `bg.surface`, so it rides the transparent
base with the list.

**2026-07-28 (ROD-484):** The §5.3a provider row encodes serving and pin on the
fg ladder instead of naming the pin in text: the `· pin {pinned}` segment is gone,
serving takes `fg`, pinned takes `fg` + bold, everything else stays `fg2`, and the
`[v]` cycle hint still trails. The pin boost is gated on a confirmed `▸`/`+`
marker so `?` can never outrank `+`; a pin on a provider outside the registry now
renders nowhere. Provider and Pinned left `detail_meta_fields` entirely (six
fields, all show metadata), `provider_line` reads the session directly, and
`MetaField` dropped `label` and `rail_only` with them: `{value, unit, dim}`.

**2026-07-20 (ROD-458):** Removed the §5.3a labeled metadata rail and its
`bloom`/`two_col` gating. The detail metadata is now one compact `·`-joined
line at every width and origin: Episodes, Format, Source, Duration, Studios,
Rank, with Rank riding last so it is the first field the line sheds when width
tightens. Provider and Pinned no longer sit with the show info; they render as
a dedicated row at the top of the episode grid (`provider_line`), formatted
`▸{serving} {bindings} · pin {pinned} · [v]`, visible only when the grid is
engaged. Live functions: `meta_line`, `provider_line`, `alt_rows`
(`src/tui/view/detail.rs`). Residual references to the removed rail and its
`bloom`/`two_col`/`rail_only`-bloom plumbing throughout this document are
struck through in place rather than deleted, with surviving conclusions kept
or annotated alongside.

