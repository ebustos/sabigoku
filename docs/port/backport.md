# Backport ledger · sabigoku → zigoku

| Field | Value |
|---|---|
| Purpose | Cases where port work fixed, hardened, or questioned something zigoku still carries at `083abd3`. The work-list for the eventual backport pass. |
| Rules | Add a row the moment a deviation is ratified or a review finding is confirmed against the freeze. Rows cite chapters instead of re-explaining. No Zigoku-module tickets until backporting actually starts; this doc becomes the filing checklist then. |

| Case | zigoku @ freeze | sabigoku | Verdict | zigoku site | Source |
|---|---|---|---|---|---|
| K-2 forced-preferred walk dead-end | Stale-stamp re-route onto a search-only preferred provider exhausts a single-provider walk and strands the grid while bindings exist | Walk-origin tag split; K-2 law in 03 §5.3 | **Backport** | `tui/resolve_state.zig`, `tui/app.zig` | 07 K-2, ROD-430 |
| Non-sync 429 classification | Any non-200 collapses to no-answer; hammering risk called out in 06 §8b | Distinct rate-limited error class, no client retry | **Backport** | `anilist.zig` `postGql` | ROD-435 |
| Control-char strip coverage | `stripControls` drops C0 + DEL only; C1, bidi overrides, zero-width pass into terminal cells (RTL title spoofing reproduced) | Widened filter: C1 + U+202A-2E + U+2066-69 + U+200B-D + U+FEFF | **Backport** | `anilist.zig` `stripControls` | ROD-435 chaos pass |
| HTTP redirect handling on catalog POSTs | `std.http.Client` redirect behavior unaudited; a followed redirect can re-route the request off-host | Redirects refused on the catalog client | **Needs check** first: confirm what zig std does at freeze, then decide | `anilist.zig` `fetchGql` | ROD-435 chaos pass |
| Unterminated `<` in description | `sanitizeDescription` never leaves tag state; rest of the synopsis silently vanishes | Same (CLONE kept for parity) | **Needs decision**; fix would apply to both | `anilist.zig` `sanitizeDescription` | ROD-435 chaos pass |
| Cover URL sanitizing asymmetry | `thumb` is the one API string with no control-strip ("validated on fetch") | `cover_url` stripped like every sibling field | **Backport** (cheap, closes the asymmetry) | `anilist.zig` `mediaToMeta` | ROD-435 chaos pass |
| Blank-romaji title guard | Seed-flag gate (ROD-312) protects canonical titles healed off provider seeds | NULLIF-at-merge; seed flag not ported (no provider seeds reach these writers) | **Skip**: zigoku's gate fits its provider-seeded identity model; ours fits AniList-first | `anilist.zig`, `store.zig` | 02 §merge, ROD-434 |
| genres/studios encoding | `'\n'`-joined TEXT | JSON string arrays (02 L5) | **Skip**: encoding choice serves each language's tooling; no product difference | `store.zig` | 02 L5, ROD-434 |
