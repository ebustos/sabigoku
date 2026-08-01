# 07 · Bug ledger

| Field | Value |
|---|---|
| Status | `review-stable` |
| Ticket | ROD-428 |
| Freeze | zigoku `083abd3` (`v0.4.6-3`, tip ROD-419) |
| Sources | `CHANGELOG.md` Fixed/Known + port decisions 02–06 + freeze tip beyond 0.4.6 tag |

## Purpose

Scar tissue so sabigoku does not re-ship bugs zigoku already paid for, and so
**known open issues** have an explicit **CLONE** vs **FIX-IN-RUST** call.

This is not every commit. Prefer user-visible or data-corrupting scars. Cite
CHANGELOG version or ROD when useful. Behavioral detail lives in [05](05-behavior-contracts.md);
this chapter is the **disposition index**.

## Disposition legend

| Tag | Meaning |
|---|---|
| `CLONE` | Ship the **fixed** behavior (do not regress to the old bug) |
| `OPEN` | Still broken / limited in zigoku at freeze; sabigoku must choose |
| `FIX-IN-RUST` | Do better than zigoku; intended new behavior named |
| `N/A` | Packaging, Zig-only, or out of product scope |

## How to use

1. Before implementing a surface, skim the matching section here + 05.
2. New zigoku fixes after freeze → **Upstream delta** (§9), not silent rewrite of rows.
3. Closing a row: move to Done only when a sabigoku test or contract cites it.

---

## 1. Identity and storage (architectural)

| ID | Symptom / trap | Disposition | Notes |
|---|---|---|---|
| ID-1 | Provider-primary library PK forced re-keys, unbound sentinels, COALESCE dual spine | `FIX-IN-RUST` | **02:** `anilist_id` is show SOT; bindings are edges |
| ID-2 | Search hits pollute library via `history_visible` | `FIX-IN-RUST` | **02 L2:** durable `catalog_cache`, no library pollution |
| ID-3 | Concurrent migrate half-applied schema | `CLONE` | Atomic ladder + busy timeout (ROD-287); 0.3.0 CHANGELOG |
| ID-4 | SQLite bind error written as silent NULL | `CLONE` | Propagate bind errors (0.1.3) |
| ID-5 | Airing availability snapshot pinned as finale `total_episodes` | `CLONE` | ROD-419 @ freeze tip; enrich must not freeze aired-so-far as total |
| ID-6 | zigoku DB import / dual-read | `N/A` | **02 L3:** independent store; importer separate issue later |
| ID-7 | Unbundled SQLite segfaults at startup on stock macOS | `CLONE` | v0.1.1 (buried in an "Added" bullet); rusqlite **bundled** (08 §1) — ledger row so the crate choice stays traceable to the crash it prevents |

---

## 2. Known issues at freeze (`OPEN`)

| ID | Symptom | Disposition | Sabigoku plan |
|---|---|---|---|
| K-1 | Resume marker one episode behind after **source switch** when labels disagree | `OPEN` (deferred UX) | **02 L1:** string equality for progress; no auto-remap in M1, so the limitation **ships**. Human-in-the-loop repair UX = **future ticket**. (Round-1 relabel: the old `FIX-IN-RUST` tag implied an M1 fix that is not scoped) |
| K-2 | First open under **search-only preferred** can show **empty grid** instead of falling back to an existing binding | `FIX-IN-RUST`; moot since ROD-525 (mechanism retired) | **03 §5.3** "after a forced-preferred miss": continue the walk / land on an existing binding; no blank dead-end. Also split the miss toast: zigoku reuses the pin-kept copy for this pinless miss |

---

## 3. Resolve, providers, playback

| ID | Symptom (old bug) | Disposition | Notes |
|---|---|---|---|
| R-1 | Preferred re-route / pin / fallback / empty listing / demote contracts | `CLONE`; re-dispositioned by ROD-525 (last-used + one walk) | Full matrix **03** + **05 §10** (ROD-343–357, 368, 398, 229) |
| R-2 | Manual flip to empty source dead-ends | `CLONE`; ROD-525: the manual walk walks on instead | 0.4.4: keep pin, fall back, name miss |
| R-3 | Backup-only shows empty grid, no walk | `CLONE` | 0.4.1 multiprovider walk before give up |
| R-4 | Empty listing bound as success | `CLONE` | ROD-368: empty walks ladder |
| R-5 | Stream open / CDN block fails hard | `CLONE` | 0.3.0 retry + toast; play retry 3× open-fail only (**04**) |
| R-6 | macOS crash on playback failure | `CLONE` | 0.3.1: surface error, no abort |
| R-7 | Softsubs missing / flaky / wrong track | `CLONE` | 0.4.3: fetch, retry, content-based track pick |
| R-8 | Quality cap ignored on fallback provider | `CLONE` | 0.4.3 |
| R-9 | Provider flip resets episode cursor | `CLONE` | 0.4.0; **05** v-flip keeps in-progress ep |
| R-10 | Malicious redirect / forged stream URL | `CLONE` | 0.4.6: SSRF / unsafe link checks on **all** resolve paths |
| R-11 | Post-playback history refresh jumps detail/cover to wrong show | `CLONE` | 0.4.6: view follows watched show |
| R-12 | Airing show auto-completes at latest aired ep | `CLONE` | 0.3.0 / ROD-296: still-airing never auto-complete |
| R-13 | Progress fraction overshoots total (e.g. 14/2) | `CLONE` | 0.3.1: clamp watched to total |
| R-14 | Partial watch counted as completed / advances cursor | `CLONE` | ROD-168 natural end 0.80 vs fully_watched 0.95 |

---

## 4. TUI, workers, Discover

| ID | Symptom (old bug) | Disposition | Notes |
|---|---|---|---|
| T-1 | Join superseded episode prefetch blocks input | `CLONE` | 0.1.3 / ROD-179: detach + keep-check |
| T-2 | Quit freezes alt-screen draining workers | `CLONE` intent | 0.1.4 zigoku hard-exit; **04** prefer clean drain without deadlock |
| T-3 | Browse task_error marks History unavailable | `CLONE` | 0.1.4 / ROD-234 |
| T-4 | Discover UI-thread fetch freezes app | `CLONE` | 0.2.2 off-thread + deadlines |
| T-5 | Discover covers blank (relative URL / WebP) | `CLONE` | 0.2.2 |
| T-6 | Discover re-title-match instead of canonical link | `CLONE` → `FIX-IN-RUST` shape | 0.2.3; sabigoku: AniList id + catalog_cache |
| T-7 | Discover axis cycle overflow | `CLONE` | 0.2.0 |
| T-8 | Discover over-fetch on huge monitors | `CLONE` | 0.4.0 page growth cap + **04** max_feed_rows 300 |
| T-9 | Episode grid bleeds Browse preview from History | `CLONE` | 0.1.2 / ROD-222 |
| T-10 | Watched dim only from History open, not Browse | `CLONE` | 0.1.2 shared seed |
| T-11 | Long episode grid stray wrong number | `CLONE` | 0.4.6 |
| T-12 | CJK wrap splits codepoints; center ignores wide cols | `CLONE` | 0.4.6 text measure |
| T-13 | Kitty `_Gi` acks bleed to shell / tmux | `CLONE` if Kitty path ships | 0.1.5 / 0.2.0 drain + quiet |
| T-14 | Cover decode peak memory | `CLONE` intent | 0.4.6 tighter ceiling; tune in M1 |
| T-15 | History filter only romaji | `CLONE` | 0.3.1 / ROD-299 all title forms |
| T-16 | Two-column detail measured the **terminal**, not the pane; borderline widths clipped genres/metadata | `CLONE` | 0.2.1 (filed as Changed, so it dodged the Fixed net); pane-width law pinned in 05 §5 |
| T-17 | History detail needed a second keypress before the episode grid appeared | `CLONE` | 0.2.1; grid renders on first focus at any width |

---

## 5. Auth, sync, config

| ID | Symptom (old bug) | Disposition | Notes |
|---|---|---|---|
| A-1 | Token with CR/LF aborts or injects headers | `CLONE` | auth control-byte refuse (**06**) |
| A-2 | Persist token before Viewer verify | `CLONE` (must not) | verify-before-persist |
| A-3 | Push-first wipes AniList on first sync | `CLONE` (must not) | pull-then-push (**06**) |
| A-4 | Sync clobber mid-edit | `CLONE` | CAS / contended skip |
| A-5 | reloadAuth frees token mid-flush | `CLONE` (must not) | 05 reloadAuth ownership |

---

## 6. Intentional product choices (not bugs)

| ID | Choice | Disposition | Notes |
|---|---|---|---|
| P-1 | Unmatched AniList list entries not auto-imported on pull | `CLONE` | 06: unmatched count only |
| P-2 | Hard delete is permanent after confirm | `CLONE` | 05 ROD-220 |
| P-3 | Windows unsupported paths | `CLONE` until ported | 06 paths |
| P-4 | AUR/Homebrew/install packaging | `N/A` | sabigoku packaging later |
| P-5 | Non-TUI CLI search→mpv | `N/A` defer | 01/06 |

---

## 7. CHANGELOG index (fixed → CLONE)

Quick map version → themes. Full prose in zigoku `CHANGELOG.md`.

| Ver | Themes to not regress |
|---|---|
| 0.4.6 | Wrong show after play; CJK wrap/center; episode grid glitch; stream SSRF; cover mem |
| 0.4.4 | Manual flip dead-end |
| 0.4.3 | Softsubs; quality on backup |
| 0.4.1 | Backup-only resolve |
| 0.4.0 | Flip cursor; search punctuation; Discover page cap; anipub migrate edge (N/A shape) |
| 0.3.1 | macOS play crash; progress clamp; filter titles |
| 0.3.0 | CDN retry; airing complete; atomic migrate |
| 0.2.3 | Discover canonical carry |
| 0.2.2 | Covers WebP/relative; Discover off-thread |
| 0.2.1 | Two-column split measured pane not terminal; History grid on first focus |
| 0.2.0 | Kitty quiet; Discover cycle overflow |
| 0.1.5–0.1.2 | Kitty drain; quit freeze; History banner; scroll join; bind errors; grid bleed; dim seed |

---

## 8. High-signal ROD clusters (regression spine)

Implement with tests citing **05**. Do not re-open without reading 03/05.

| Cluster | Topic |
|---|---|
| ROD-179 / 156 | Supersede workers; no UI join |
| ROD-182 / 278 | Enrichment stamp + answered |
| ROD-189 / 193 | P-add, undo, recompute |
| ROD-205 / 299 | Title language + filter |
| ROD-210 / 249 | Esc/q/view leave matrix |
| ROD-220 | Hard delete confirm |
| ROD-229 / 259 | Resume landing + demote |
| ROD-234 | Browse vs History errors |
| ROD-239–243 / 336 / 339 | Discover feed/covers/canonical |
| ROD-284–286 / 291 | Sync push/pull/connect/flush |
| ROD-296 / 419 | Still-airing totals |
| ROD-309 | Play open retry |
| ROD-328–329 / 343–357 / 368 | Binding tiers, unbound, walk |
| ROD-345 / 347 / 398 | Pin, absence, preferred route |
| ROD-370 | Update check |

---

## 9. Upstream deltas (after freeze)

Freeze tip already includes **ROD-419** (stale totals). Log newer zigoku fixes here when amending:

| Date | zigoku rev | Note | Action |
|---|---|---|---|
| 2026-07-17 | `083abd3` | Freeze | — |
| | | | |

---

## 10. Adversarial checklist (ROD-430)

- [x] K-1 and K-2 have explicit sabigoku plans (K-2 retired outright with its mechanism, ROD-525)
- [ ] ID-1/ID-2 match 02 locked decisions
- [ ] No row requires reintroducing `SOURCE_UNBOUND` or provider-primary PK
- [ ] Playback/resume scars point at 05/03
- [ ] Security (R-10, A-1) not marked N/A
- [ ] Packaging not blocking M1

---

## 11. Primary sources

- zigoku `CHANGELOG.md` @ freeze
- This bible 02–06 disposition tags
- `app_test` / store tests (via 05)
- Plane ROD ids as index only
