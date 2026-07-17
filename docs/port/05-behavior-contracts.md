# 05 · Behavior contracts

| Field | Value |
|---|---|
| Status | `draft` |
| Ticket | ROD-426 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Spine | Identity [02](02-domain-and-sqlite.md) · Resolve [03](03-providers-and-resolve.md) · UI [DESIGN.md](../../DESIGN.md) |
| Primary cite | `src/tui/app_test.zig` (~321 tests) unless noted |

## How to read this chapter

Each contract is **product law** for sabigoku. Cites are zigoku tests at the freeze
rev. Where zigoku still thinks in `(source, source_id)` rows, the **intent** ports
to `show` / `provider_binding` (02); do not reintroduce provider-primary identity
to make a test compile.

| Tag | Meaning |
|---|---|
| `CLONE` | Same user-visible behavior |
| `PORT` | Same intent; storage/API shape follows 02/03 |
| `UI` | Visual/layout; DESIGN is authority, tests are regression pins |
| `DEFER` | Covered primarily in 02/03/06/07; listed for traceability |

Format: **Given / When / Then / Must not**, plus cites.

---

## 0. Port translation (read once)

| zigoku test language | sabigoku law |
|---|---|
| History row / `anime` record | Library **`show`** (`anilist_id`) |
| Sibling bindings / union progress | Multiple **`provider_binding`** edges; progress on show (02 L1) |
| `SOURCE_UNBOUND` / empty grid unbound | Show with **no** bindings; UI "no source" |
| Provider-keyed `.direct` add | Explicit binding context or bound open (03) |
| `history_visible = 0` search pollution | **`catalog_cache` only** until engage (02 L2) |
| Canonical persist without binding | `catalog_cache` and/or `show` without binding |

Module-level store/resolver tests (store.zig, resolver.zig, source.zig, domain.zig)
remain **law** for their domains; this chapter focuses on **TUI + cross-cutting**
behavior. Pointers:

- Domain enums, afterPlay, isStillAiring, titles → domain tests + 02 §4
- Schema, pins, absences, routes, migrate → store tests + 02
- Registry order, tier match floors → source/resolver tests + 03
- Sync push/pull/reconcile → 06 (cites below only when TUI arms them)

---

## 1. Navigation, quit, Esc chain

### 1.1 List motion

| Contract | Cites |
|---|---|
| **G** j/k stay in bounds; g/G jump ends; empty history is a no-op | `j/k navigation stays in bounds`, `g/G jump to ends`, `navigation is a no-op with empty history` |
| **G** scrollIntoView keeps cursor in viewport; degenerate tops do not corrupt | `scrollIntoView…`, history 2-row scroll tests |

### 1.2 Quit

| | |
|---|---|
| **Given** normal mode on Browse/History/Settings | |
| **When** `q` or Ctrl-C | |
| **Then** app quits (Settings: save if dirty first when path exists) | |
| **Must not** treat `q` as back-nav from detail, zoom, or History | |

Cites: `quit keys: q from browse and Ctrl-C`, `q from history quits…`, `q from settings saves…`, `q from a browse detail pane quits…`, `q from the zoom quits…`, `q from a focused History detail pane quits…`, `q typed into a Browse search appends…`, `q typed into a History filter appends…`, Ctrl-C dirty Settings save.

### 1.3 Esc chain (`CLONE` DESIGN §Esc)

| Context | Esc does | Cite cluster |
|---|---|---|
| Browse detail → list | return list pane | `Esc from browse detail pane returns to list pane` |
| Browse list | no-op | `Esc from browse list pane is a no-op` |
| History detail → list | list, **not** Browse | `Esc from history detail…` |
| History list | no-op (stay History) | `Esc from history list is a no-op` |
| Settings | no-op, **never** save | `Esc from settings is a no-op…` |
| Zoom | demote to two-pane detail (or list if below split) | zoom demote tests ROD-170 |
| Search / filter active | clear mode (not quit) | filter/search esc tests |

### 1.4 Pane focus (Browse)

| | |
|---|---|
| **When** width ≥ 60 | h/l (and arrows) move list ↔ detail; edges no-op |
| **When** width < 60 | l is no-op (no second pane); Enter/Space open zoom |

Cites: h/l browse tests, single-column ROD-194, arrow parity ROD-156.

---

## 2. History cursor and `setHistory`

| Contract | Port note | Cites |
|---|---|---|
| Cursor follows **focused show identity** across reorder, not raw index | Identity = `anilist_id` | `setHistory follows the focused show across a reorder…` |
| With active filter, anchor is **filtered ordinal** | | `setHistory anchors to the FILTERED ordinal…` |
| Anchored show filtered out → clamp | | `setHistory falls through to the clamp…` |
| Out-of-range cursor clamps | | `setHistory clamps…` |
| Shared cursor left alone outside history context | | `setHistory leaves a shared cursor alone…` |
| Cursor walks **group order** (watching→planning→paused→completed→dropped), not store order | | `history cursor walks §5.4 group order…` |
| Geometry counts group headers, hairlines, inter-group blank | `UI` | `history geometry counts…` |
| Filter matches **any** present title form | | `history filter matches any present title form…` |
| Filter reduces nav_len; Esc clears filter + resets cursor | | `history filter: reduces…`, `esc clears…` |

---

## 3. Hard delete (ROD-220)

| | |
|---|---|
| **Given** focused library show, not currently playing | |
| **When** `X` then `y` | |
| **Then** show deleted with cascade (progress, cache, bindings, pin/absence/route); cursor held sensibly | |
| **Must not** delete on `X` alone; `q` does not confirm; re-`X` re-arms; Esc/other cancels; only `y` fires | |
| **Must not** delete the currently-playing show | |
| **When** only show deleted | first-run / empty history state |
| **When** background reload while confirm armed | confirm cancels |
| **When** delete confirms | pending status-undo is nullified |

Cites: full ROD-220 cluster in app_test. Confirm chrome width drift guard is `UI`.

**Port:** delete is by `anilist_id` on `show` CASCADE (02), not by provider PK.

---

## 4. Status, undo, recompute, add (ROD-139 / 189 / 193)

| Contract | Cites |
|---|---|
| History p/x/c/w transition status in store + memory | `History p/x/c/w keybinds…` |
| Browse `P` adds planning watchlist entry | `Browse P adds… planning` |
| History `P` re-plans + undo | `History P re-plans…` |
| `u` undoes last status mutation (store + memory) | `History u undoes…` |
| `r` recomputes progress from episode_progress; recompute-to-0 clears resume marker | `History r recomputes…`, `recompute-to-0 clears…` |
| After `c` then `r` then `u`: recompute survives; undo no-op | `History c then r then u…` |
| action-sync arms debounce on status/undo/finished ep when connected | ROD-291 action-sync tests |

**Store-level** afterPlay / still-airing completion: domain + store tests (02 §4–5). `DEFER` detail but **must** hold in TUI after play.

**Port:** P-add writes `show` + optional binding; never a provider-only library row. Discover/Browse cards that are AniList-keyed use catalog → promote (02 L2). zigoku "provider-keyed card adds directly" becomes "already has binding" or "tier path from 03", not a second identity.

---

## 5. Layout gates (`UI` + regression pins)

| Gate | Law | Cites |
|---|---|---|
| Too-small terminal | bail if **either** arm trips | `layout bails when EITHER too-small…` |
| pane_split_min **60** | two-pane Browse/History; in-pane episode grid; Enter plays from pane | two-pane / ROD-170 / ROD-259 tests |
| List width | ~38% with **30-col floor** | `paneSplit holds…`, `clamps…30-col` |
| Detail two-column **100** | keyed on **pane** width, not terminal | ROD-113 / ROD-258 |
| History preview split | only with focused record | `history preview split engages only…` |
| F1/B from History | reset viewport before Browse reads it | `F1/B from History reset the viewport…` |

DESIGN owns mocks; these numbers are **shipped constants** unless DESIGN + this chapter change together.

---

## 6. View switching

| Bind | Law | Cites |
|---|---|---|
| F1 / B | Browse; no-op if already there (preserve pane where tested) | F1/B cluster |
| F2 / H | History; H is **goto not toggle** | `H is a direct goto…` |
| F3 / D | Discover; no-op if already there | Discover open tests |
| F4 / S | Settings; no-op if already there | Settings open tests |
| Search mode | letter binds that switch views are **inert** (append to query) | `D in search mode…`, `S in search mode…` |
| Dirty Settings leave | B/H/D/F-keys **persist** on the way out | `B/H/D + F1/F2/F3 from a dirty Settings…` |

---

## 7. Discover

| Contract | Port | Cites |
|---|---|---|
| Feed lands per **axis** slot; `hasNextPage` drives exhaustion | | `discover_feed lands…` |
| Multi-axis fan-out balances drain; pages file per-axis | | `overlapping Discover feeds…` |
| Retention cap can exhaust while AniList has more | | `feed retention cap…` |
| Enter Discover: fresh slot = cache hit; stale = fetch | Use **catalog_cache** + in-memory axis slots | `entering Discover with a fresh slot…` |
| Cursor l / g/G; axis keys reset cursor; wrap both ends | | grid/axis tests |
| Enter → detail zoom; Esc returns | | `Discover Enter opens…` |
| Cover pump: at most cap new fetches; leave room for in-flight | | `pumpDiscoverCovers…` |
| Prefetch next page near end; blocked if exhausted/loading | | `Discover prefetches…` |
| `/` jumps to Browse search | | `Discover / jumps…` |
| Feed error: axis failed, spinner cleared | | `discover_feed_error…` |
| Persist rows **canonically** (AniList id), never as provider bindings | **02/03** | `discover_feed persists rows canonically…` |
| Enter resolves AniList card via **binding**, never raw provider id confusion | **03** | `Discover Enter resolves an AniList-keyed card…` |
| P on AniList card reveals/binds, no bogus row | **03** | `Discover P on an AniList-keyed card…` |
| This Season gated on live clock | | `This Season fetch is gated…` |
| NEW chip: current-cour match | `UI` | `isNewRelease…` |
| Season chip from enriched card | catalog_cache fields | `topBarSeasonChip…` |

---

## 8. Enrichment refresh

| | |
|---|---|
| **When** enrichment_refreshed with answered=true | overwrite drift fields, stamp freshness, **preserve user state** |
| **When** answered=false | skip stamp and persist |

Cites: `enrichment_refreshed overwrites…`, `answered=false skips…` (ROD-182/278).

**Port:** library enrich updates `show`; list/search enrich updates `catalog_cache` (and show if already library). Never clobber list_status/progress via enrich.

---

## 9. Titles (ROD-205)

| Contract | Cites |
|---|---|
| Primary title follows `title_language` with fallback chain | `detailRenderInfo resolves the primary title…`, domain `preferredTitle…` |
| History filter searches all present forms | `history filter matches any present title form…` |

---

## 10. Resolve, preferred, pin, fallback, prewarm

Full pipeline law is **03**. These TUI contracts pin orchestration scars:

### 10.1 Owning provider / open

| Contract | Cites |
|---|---|
| History open fetches on **owning** provider, not always primary | `ROD-343: a history row fetches…` |
| Unregistered source falls back to default provider | `…unregistered source falls back…` |
| Browse open dispatches to verdict's provider | `browse open dispatches…` |
| Browse scroll does **not** fetch episodes; detail entry lazy-loads | `browse scrolling fires zero episode fetches…` |
| Superseded episode prefetch abandoned, not joined | `ROD-179: a superseded episode prefetch…` |

### 10.2 Preferred re-route (ROD-398)

| Contract | Cites |
|---|---|
| Unpinned History open re-routes off stale binding to preferred | `ROD-398: an unpinned History open re-routes…` |
| Pin ignores global preference | `…pinned show ignores…` |
| Settled under pref opens pref binding directly | `…already settled…` |
| Browse open of bound canonical honors preference | `…Browse open…` |
| Settled pref never bound → fall back **without** re-search loop | `…never bound falls back without re-searching…` |
| Stale + search-only preferred → Tier C once | `…forces a search-only preferred…` |
| Re-route carries progress (sibling union → show progress) | `…carries progress…` **PORT** to single show progress |
| Auto-resume onto stale Tier-C still arms demote | `…still arms the demote contract…` |

### 10.3 Fallback / absence / empty (ROD-346/347/368)

| Contract | Cites |
|---|---|
| Failed fetch hops to next tier-A | `…hops to the next provider's tier-A…` |
| Hop reuses sibling binding before probe | `…reuses an existing sibling…` |
| Fresh absence skipped; stale re-probes | `…skips a fresh-absent…` |
| Bound outranks fresh negative | `…tier-0 sibling… outranks…` |
| Exhausted walk → dead-end toast, free walk | `…exhausted walk falls through…` |
| Landed hop mints under hop provider, clears walk | `…landed fallback grid mints…` |
| Virgin tier-A fail walks via pending_bind | `…virgin tier-A probe failure…` |
| Empty listing walks ladder; does not bind empty grid | `ROD-368: an empty listing walks…` |
| Empty + no other provider → no-source state | `…concedes the unbound state` **PORT** no sentinel row |
| mapEpisodeIndex: raw then ordinal else null | `mapEpisodeIndex prefers…` |
| Stream fail relaunches hop; one shot per provider per walk | `…never-played stream failure relaunches…` |
| Resume walk exhaust on tier-C miss demotes History | `…resume-landing walk that exhausts… demotes…` |
| Superseded resolve_play_target dropped | `…superseded resolve_play_target success is dropped…` |

### 10.4 Prewarm (ROD-351/348)

| Contract | Cites |
|---|---|
| Candidates = unchecked only (not bound, not fresh-absent) | `prewarmCandidates keeps only unchecked…` |
| Results mint hidden bind / negative; done clears guard | `prewarm_result mints…` **PORT** hidden bind → binding without forcing library chrome if applicable; prefer binding rows only |
| Prewarm refreshes open show availability rail | `prewarm write refreshes…` |
| Add success triggers warm; busy/repeat silent | `an add success triggers the warm…` |
| Cancel flag honored | `prewarmTask… honors the cancel flag` |

### 10.5 Pin cycle `v` (ROD-345/355/357)

| Contract | Cites |
|---|---|
| Cycle unpinned → each provider → unpinned | `v cycles unpinned -> alpha -> beta…` |
| History open redirects to pinned sibling binding | `History open redirects to the pinned…` |
| Pin leads automatic fallback order | `the pin leads the automatic fallback walk's order…` |
| Retired pin name → unpinned, no re-route | `v with a retired… wraps to unpinned…` |
| Flip keeps cursor on in-progress episode | `v flip landing keeps the cursor…` |
| Flip onto unbound provider does fresh resolve | `v onto an unbound provider resolves fresh…` **PORT** no binding |
| Recover from failed-flip unbound via focused show | ROD-357 cluster |
| Manual tier-C miss toast names **target** provider | `…names the flipped-to provider…` |

### 10.6 Resume landing (ROD-229 / 259)

| Contract | Cites |
|---|---|
| Target = most-recently-watched row, else null | `resumeTargetIndex…` |
| Failed auto-open demotes to History | `failed resume grid fetch demotes…` |
| User-driven episode fail **stays** in detail | `user-driven episode fetch failure stays…` |
| Seeds grouped ordinal so meta+grid one show | `resume landing seeds the grouped ordinal…` |
| At 60–99: in-pane grid, no forced zoom | `resume landing focuses the in-pane grid…` |
| Fires only on **first** history load, not reload | `…only on the first history load…` |
| Never-played + last_watched landing stays History | `never-played history under last_watched…` |
| Successful load clears demote arm | `successful resume grid load clears…` |

---

## 11. Playback session

| Contract | Cites |
|---|---|
| position_update refreshes live fields; checkpoint ~30s | position_update tests |
| play_done: meaningful final persists; non-meaningful keeps checkpoint | play_done cluster |
| Partial watch (below natural end): record play, **not** progress/dim/advance | ROD-168 partial tests |
| Completed position (incl. play_error): record, advance cursor, dim | completed play_error tests |
| Final episode: stay put + "all caught up" toast | `play_done on the final episode…` |
| No observed position: no recordPlay; no cursor advance; keep checkpoint | no-position tests |
| Failure class copy: source + player-spawn | `episodes_error names…`, `play_error names…` |
| play_retry warn toast during backoff | `play_retry surfaces…` |
| Playback for **other** show must not advance detail; still toast errors | cross-show tests |
| Double firePlay while playing is no-op | `firePlay: double-play guard…` |

Resume ratios and fully_watched: **02** / store tests.

---

## 12. Covers

| Contract | Cites |
|---|---|
| Decision table: none / fetch / clear / up_to_date / suppress cooldown | entire `cover decision:` cluster |
| Failure cooldown per id+url; url change recovers; other id does not suppress | same |
| Live pixels win over stale failure | `live pixels win…` |
| halfBlockFit geometry | halfBlockFit tests (`UI`) |
| Discover cover pump / adopt / error cooldown | discover cover tests |
| Detail cover_done stale discarded; error clears for retry | cover_done/error tests |

**Port:** cover keys should be `anilist_id` (and url), not provider ids.

---

## 13. Settings

| Contract | Cites |
|---|---|
| Cycle presets wrap; out-of-preset snaps valid | settings cycle tests |
| translation / palette / landing live-sync | matching tests |
| preferred provider: unset → each registry name → unset; unknown re-enters at unset; empty names inert | ROD-344 provider row tests |
| Bool toggles: AniList sync, update-check | ROD-286 / ROD-370 toggles |
| Connect row is action (not cycle) | connect row test |
| mpv_path edit: enter/type/confirm; Esc cancels; empty never commits blank | mpv_path tests |
| j/k clamp interactive rows | navigation test |
| q dirty without path: warn then quit | ROD-210 |
| Save round-trip on q | save round-trip |
| Enter settings resets cursor/edit/mode | entering settings… |
| Edit mode swallows F-keys | edit mode swallows… |
| Ctrl-C hard-quits even while editing | Ctrl-C while editing |
| reloadAuth must not free token still used by in-flight flush | `reloadAuth retires arenas…` → **PORT** ownership/Arc |

---

## 14. Toasts and async chrome

| Contract | Cites |
|---|---|
| Topic singleton; persistent error not evicted by two-way sync whispers | pushToastTopic tests ROD-293 |
| Transient overflow evicts oldest | all-transient overflow |
| task_error persistent; Browse failure never marks History unavailable | task_error / ROD-234 |
| History load fail banner; success clears | history_load_failed cluster |
| Copy budget truncation with ellipsis | task_error truncates… |
| Sync flush whispers; order ↓ then ↑ | sync_flushed tests |
| update_available low-key whisper | update_available… |

---

## 15. Detail chrome / provider caption

| Contract | Cites |
|---|---|
| Meta field order and `?` degrade | ROD-260/261 meta tests |
| Provider caption: serving leads; markers; dim; shed order vs Pinned | ROD-348/356/397 |
| Browse preview hides stale episode grid from History | ROD-222 |
| episodeGridVisible in zoom | ROD-222 |
| detailSyncTarget rules for browse/history | ROD-156 |

---

## 16. Search / command input

| Contract | Cites |
|---|---|
| Search mode: chars append + debounce; h/H append not navigate | search mode tests |
| tick advances spinner; fires debounced search past deadline | tick tests |

---

## 17. Cross-cutting store contracts still enforced via TUI

These are easy to break in the port if only unit-tested in isolation:

| Law | Why TUI cares |
|---|---|
| Upsert/enrich never clobbers user state | P-add, re-search, enrichment_refreshed |
| Progress on show, not forked per provider | preferred re-route "carries progress" |
| catalog_cache for Discover/Browse paint | feed persist + detail open without refetch |
| Pin/absence/route off enrichment path | caption + resolve tests |

---

## 18. Inventory completeness

| Source | Count (freeze) | In this chapter |
|---|---|---|
| `app_test.zig` | ~321 | Grouped above; every ROD-tagged cluster named |
| store / domain / source / resolver / sync / config / auth | many | Pointed via §0 and 02/03/06 |

**Not expanded here (by design):** pure render pixel assertions beyond halfBlockFit;
Kitty protocol; packaging. Add cites when M1 hits those modules.

If a new app_test lands on zigoku after freeze, either amend freeze or add an
**upstream delta** note in 07; do not silently expand this chapter against master.

---

## 19. Adversarial checklist (ROD-430)

- [ ] Esc/q matrix has no ambiguous cell
- [ ] setHistory identity is anilist_id, not provider row
- [ ] Delete/cascade matches 02 tables
- [ ] Discover never writes provider-primary library rows
- [ ] Every 03 orchestration scar in §10 has at least one cite
- [ ] Resume demote only on auto path
- [ ] Playback partial vs completed matches natural-end ratio
- [ ] No contract requires `SOURCE_UNBOUND` or dual-spine COALESCE
- [ ] Settings preferred-provider wheel matches registry injection

---

## 20. Suggested sabigoku test map (non-binding)

When implementing, prefer **one Rust test module per section** (1–16) over a single
mega-file, but keep the **same contract titles** so this chapter stays the index.
