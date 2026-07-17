# 04 · TUI runtime

| Field | Value |
|---|---|
| Status | `review-stable` |
| Ticket | ROD-425 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Depends on | [02](02-domain-and-sqlite.md) identity/cache · [03](03-providers-and-resolve.md) resolve · [05](05-behavior-contracts.md) must/must-not · [DESIGN.md](../../DESIGN.md) views |
| Nature | **Port design** for the event loop and workers. Product rules cite 05; resolve cites 03. |

---

## 1. Architecture (one sentence)

**One UI thread** owns terminal I/O and all mutable TUI state. Background work
runs on **detached workers** that only **post events** into a thread-safe queue.
`tick(event)` mutates; **draw is pure** from state (zigoku comment: *tick mutates
state; draw is pure*).

```
┌─────────────┐   keys/winsize    ┌──────────────┐
│  terminal   │ ───────────────► │              │
└─────────────┘                   │  UI thread   │──► render frame
                                  │  tick(event) │
┌─────────────┐   post(Event)     │              │
│  workers    │ ───────────────► │  mpsc/queue  │
└─────────────┘                   └──────────────┘
```

Never block the UI thread on network or mpv wait. Never join a superseded worker
from the hot path (detach + generation / keep-check drop).

---

## 2. Runtime choice (sabigoku)

| Option | Verdict |
|---|---|
| `std::thread` + `mpsc` (or equivalent channel) | **Decided** (M1 cut 2026-07-17, ROD-431/433; matches M0 `spike_concurrency`, zigoku model, and `spike_cover`'s ThreadProtocol wiring) |
| tokio multi-thread runtime for everything | Rejected for M1; revisit only on measured pain |
| async TUI framework that owns the loop | Avoid unless measured win; keep ratatui draw pure |

DESIGN 11.1 is resolved by this decision: blocking workers +
channel into a crossterm/ratatui loop, with a **~100ms tick** for spinner, debounce,
and sync flush deadlines.

Play is a long-lived worker (mpv wait + IPC), not an async task that parks the UI.

---

## 3. Main loop phases

Mirror zigoku `run` / `tick` intent:

1. **Init** terminal, install resize, drain query leftovers (spurious keys), first
   winsize, construct `App` state, open store (already migrated).
2. **Bootstrap** async: load history worker; optional launch AniList pull; optional
   update check; optional resume-landing arm (05 §10.6). Resume-landing arms from
   **two** call sites in zigoku (the sync-fallback branch and the
   history-load-done handler); cover both.
3. **Loop** until quit:
   - Wait/poll for next `Event` (key, winsize, worker message, or tick).
   - `tick(event)`: mutate state, maybe spawn workers, push toasts.
   - If dirty: **draw** full frame from state (no I/O except terminal write).
4. **Quit — the production path.** Verified at freeze: zigoku's **only** way out
   of a user quit is minimal cleanup (graphics clear, terminal restore,
   `quitFlush`) then `std.c._exit(0)`. The drain-defers are real code but run only
   in tests and error unwind; the source comment says so outright.
5. **Teardown (tests / error unwind in zigoku):** cancel connect/prewarm, drain
   all worker barriers, free owned state. If sabigoku chooses a clean drain on
   quit, that is a **deliberate deviation**, not a clone: it must keep the
   quitFlush semantics (§11) and must not deadlock on a full event queue (§8).

---

## 4. Event taxonomy

Unified enum (names indicative). Workers post only these; UI never calls provider
APIs on the hot path except spawn.

### 4.1 Terminal / time

| Event | Role |
|---|---|
| `Key` | Input (see input modes §9) |
| `Resize` | Layout recompute; pixel metrics for covers |
| `FocusIn` / `FocusOut` | Optional; keep if crossterm exposes |
| `Tick` | ~100ms: spinner, search debounce, cover settle, sync flush deadline |

### 4.2 Library / catalog

| Event | Ownership into UI | Notes |
|---|---|---|
| `HistoryLoaded` / `HistoryReloaded` | Slice owned by load arena or `Vec` transferred | Reload must not wipe list on soft fail (05) |
| `HistoryLoadFailed` | error string | Banner only; never from Browse task_error |
| `HistoryReloadFailed` | unit | Keep current slice |
| `SearchDone { results, query, page }` | owned results + query | Stale if query ≠ current |
| `TaskError` | message | Browse/search/enrich toast only |
| `DiscoverFeed { axis, page, results, has_next }` | owned rows | Per-axis slot; **catalog_cache upsert** (02 L2) |
| `DiscoverFeedError { axis, cause }` | POD | Axis failed UX only |
| `EnrichmentRefreshed { show, answered }` | owned | Persist only if `answered` (05 §8) |

### 4.3 Resolve / episodes / prewarm

| Event | Notes |
|---|---|
| `ResolveAddResult` | ok → bind + user state; absences always persist; source name is **binding owner** |
| `ResolvePlayTarget` | ok → fire episodes; staleness vs `play_resolve_aid`; absences persist even if drop |
| `PrewarmResult` / `PrewarmDone` | hidden bind or absence; single-flight clear |
| `EpisodesDone { episodes, for_id }` | keep-check vs `EpisodeState.for_id` |
| `EpisodesError { cause, for_id }` | drop if superseded (ROD-179) |

### 4.4 Covers

| Event | Notes |
|---|---|
| `CoverDone { rgba, w, h, for_id }` | detail cover; drop if id stale |
| `CoverError { for_id }` | cooldown path |
| `DiscoverCoverDone { url, rgba… }` | **URL-keyed**, no window stale-drop |
| `DiscoverCoverError { url }` | cooldown per url |

### 4.5 Playback

| Event | Notes |
|---|---|
| `PositionUpdate` | live bar; checkpoint every 30s in session |
| `PlayDone` | optional final position |
| `PlayError { final?, cause }` | class copy 03/05; may still complete watch |
| `PlayRetry { attempt, max }` | warn toast during CDN backoff |

### 4.6 Auth / sync / update

| Event | Notes |
|---|---|
| `SyncFlushed { pushed, reconciled, expired }` | whispers; reload if reconciled; disconnect if expired |
| `ConnectResult` | never post on cancel (UI tearing down) |
| `UpdateAvailable` | low-key toast |

---

## 5. Worker drain contract (`ThreadDrain` → Rust)

zigoku accounts every detachable worker with begin/finish/drain. **Intent ports:**

| Rule | Detail |
|---|---|
| **Begin before spawn** | Increment inflight on UI thread **before** `spawn`. On spawn failure, finish immediately. |
| **Finish after last post** | Worker defers finish so `drain` seeing zero means no further **touch of the loop, allocator, or io** (the guarantee is broader than "no post"). |
| **Drain only on teardown** | Not on supersede. Supersede = detach + keep-check drop. |
| **Exact-fit owned payloads** | Strings/slices posted to UI are fully owned buffers the UI frees (or takes). No partial free of a subslice (`ZIG-SHAPE` → `String`/`Vec`/`Bytes`). |

### 5.1 Drain families (at least)

| Drain | Workers |
|---|---|
| history load | **not** a ThreadDrain in zigoku: plain thread, cancelled via a store-level SQLite interrupt of the in-flight SELECT, then joined; its cleanup is declared to run after the other drains (ordering-sensitive) |
| `episode` | episode list fetch |
| `enrich_refresh` | refresh-on-view |
| `resolve_add` | tier-A/C add |
| `resolve_play` | tier-C play search |
| `prewarm` | sibling warm walk |
| `discover_feed` | per-axis pages (accounted even when detached) |
| `discover_cover` | cover fan-out |

Play worker: either its own drain or explicit join on quit after cancel signal; must
not block forever if the event queue is full (drop posts on shutdown).

---

## 6. Staleness / keep-check (generation tokens)

**Product:** late results must not clobber a newer navigation target (05, ROD-179).

| Domain | Token | Drop when |
|---|---|---|
| Episodes | `(provider, provider_id)` or show+binding key in flight | `for_id`/`for_source` ≠ event |
| Play resolve | `play_resolve_aid: anilist_id` | aid ≠ focused show |
| Search | query string | query ≠ current buffer |
| Detail cover | `anilist_id` (port; zigoku used show id string) | id ≠ current target |
| Discover cover | **url** | never drop for "wrong card" if url still needed; slot adopts by url |
| Discover feed | axis + page + loading flag | file into axis slot; outdated page discarded by slot logic |
| Prewarm | cancel flag + active guard | fallback/user resolve cancels walk |

**Rust lean:** `AtomicU64` generation per subsystem, or compare owned keys as zigoku
does. Prefer generation for episode/search to avoid string compare bugs.

**Never join** the old episode worker when starting a new fetch.

---

## 7. Subsystems (state ownership)

Embed-by-value controllers (zigoku carve-outs). Transport (drains, deadlines) may
live on `App`; records stay modular.

### 7.1 `EpisodeState`

- Owned episode list + `for_id` / `for_source` (binding key) + cursor + progress high-water + `resume_idx`.
- Two-tier cache: hot LRU (cap 8 @ freeze) + SQLite `episode_cache` (02).
- `unbound` / no-source flag: **PORT** to "no binding / empty walk conceded" without
  sentinel provider row (02/03).
- Loading spinner only after spawn strings committed (OOM must not strand spinner).

### 7.2 `ResolveTransport` (03)

- `add_resolving` / `play_resolving` single-flight.
- `pending_bind: Option<anilist_id>`.
- `fallback: Option<FallbackWalk>` parked across hop fires.
- Product rules: **03** and **05 §10**.

### 7.3 `CoverState` (detail)

Decision table is **05 §12**. Runtime:

- Fetch worker uses provider `cover_request` + HTTP; fetch guard (03 §6.7) with
  **redirects disabled** (a followed 3xx would bypass the host check).
- Dual LRU: raw bytes (~32 MiB cap) + decoded pixels (~48 MiB) @ freeze; tune in M1.
- Cache races (`CLONE` rules): clone a cached buffer **inside the same critical
  section as the insert** (an unlocked dupe races evict); disk-cache writers use a
  per-writer unique temp path before the atomic rename (a shared `.tmp` for the
  same url tears the file).
- Cooldown on same id+url failure (**10s** @ freeze); url change retries.

### 7.4 `DiscoverCovers`

- URL-keyed slots: idle / loading / ready / failed.
- Pump: start at most **cap** new fetches per tick; **leave room** for in-flight
  (05 pump tests). Cap from config `discover_cover_concurrency` (clamped).
- Visible cards preferred; skip already loading/ready.

### 7.5 `DiscoverState`

- **Four independent axis slots**: `trending`, `popular`, `top_rated`,
  `this_season` (freeze enum order; UI tab order matches). Never one shared list
  re-sorted (rank is positional per axis). Feed page size **20** (06 §8b).
- Per slot: results, page, fetched_at, loading, failed, exhausted.
- **max_feed_rows = 300** → force exhausted (ROD-339).
- On feed page success: append, set has_next, **upsert `catalog_cache`** for each
  row (02 L2). zigoku `upsertCanonicalOnly` maps here, not provider bindings.
- Prefetch next page near end when not exhausted/loading (05).

### 7.6 `PrewarmState`

- Single active walk; cancel atomic for fallback yield.
- 32-slot ring of attempted `anilist_id`s (eviction can re-attempt) + **30s
  app-wide spacing floor** between walk starts (03 §6.5). Rust: use `Option`, not
  a 0 sentinel; nothing enforces `anilist_id > 0`.
- Blocked while add/play resolving or fallback active (03).

### 7.7 `PlaybackSession`

- Owns source name, **show key**, episode raw, 1-based index, translation snapshot.
- **Port show key:** `anilist_id` (+ binding provider for resolve), not provider PK alone.
- Checkpoint every **30s** via store progress (02 episode_progress).
- `finish`: meaningful position → save progress; `completed` (natural end 0.80) gates
  high-water only (05 §11).
- Double-play guard on App: ignore fire while `playing`.

### 7.8 Play worker

- Resolve stream → mpv (IPC positions) → `PlayDone` / `PlayError`.
- **MAX_PLAY_ATTEMPTS = 3**; retry only `MpvOpenFailed` with no meaningful play yet;
  backoff 2s then 4s; re-resolve fresh URL each attempt; `PlayRetry` toast.
- AniSkip prepared once on worker before attempts (03 §9).

---

## 8. Debounce and deadlines

| Mechanism | Period / rule |
|---|---|
| UI `Tick` | ~100ms (design choice, not a named const in zigoku) |
| Search debounce | **300ms**, armed on edited keystroke; fire when `now >= deadline` |
| Cover settle (browse/history scroll) | **150ms** debounce for continuous scroll; discrete nav may sync immediately (05 cover settle) |
| Cover retry cooldown | **10s** per id+url (shared by detail and discover covers) |
| Sync flush | arm on mutation; fire after **3000ms** settle on tick (ROD-291) |
| Slow spinner color | after **3000ms** async_start age (visual; DESIGN) |
| Prewarm spacing | **30s** app-wide floor between walk starts (§7.6) |
| Toasts | queue cap **3**; persistent = per-topic singleton refreshed in place; non-persistent evicted oldest-first, compacted; copy truncated to **36 display columns** (width-aware, not chars) |
| Too-small terminal | `h < 4 or w < 16` → degraded message frame, still renders; **not** a bail/exit |

---

## 9. Input modes (runtime, not key list)

Full binds: DESIGN + 05. Runtime modes that gate dispatch:

| Mode | Workers / notes |
|---|---|
| Normal | view switches, play, resolve |
| Search | letters append; view letters inert; debounce search worker |
| History filter | same append rules; `q` not quit |
| Command (`:`) | as DESIGN |
| Settings edit field | F-keys swallowed; Ctrl-C hard quit |
| Delete confirm | only y / cancel set (05 §3) |
| Connect modal | loopback worker; cancel skips `ConnectResult` post |

---

## 10. Catalog cache integration (02 L2)

| Trigger | Store write |
|---|---|
| `DiscoverFeed` page applied | upsert each AniList row into **`catalog_cache`** |
| Browse `SearchDone` (AniList search) | same |
| `EnrichmentRefreshed` answered | upsert **`show`** if library; always may refresh catalog_cache |
| Detail open | **read catalog_cache first**; network enrich only on miss/stale/explicit refresh |

Detail card must not require a full AniList round-trip when the feed/search already
paid for metadata.

---

## 11. Teardown and quit

| Requirement | Intent |
|---|---|
| Drain all accounted workers | no use-after-free on store/gpa (if the port drains at all; see §3) |
| History load thread | interrupt the in-flight SELECT (store-level) **before** join, or teardown blocks on a slow query |
| Cancel prewarm + connect | cancel flag; connect skips post on cancel |
| Playback | best-effort final checkpoint; abandon mpv if hard exit |
| Dirty settings | save on q / view leave (05 §13) |
| **quitFlush** | last act before exit: bounded best-effort push of dirty AniList rows; **skipped when a sync worker is inflight** (never push alongside a pull, ROD-285/294) |
| quitFlush timeout | bound the wait on a **pool-independent clock** (zigoku: libc nanosleep, not the io-pool deadline): under pool starvation a pooled deadline never arms and the quit hangs (ROD-232) |
| Event queue full at quit | workers must not block forever on post (try_post / drop on shutdown) |
| Terminal restore | always |
| Kitty graphics | quiet clear (`q=2`) so the terminal does not ack deletes onto the shell after exit; drain residual tty responses only if an image was actually transmitted |

zigoku quits via `_exit(0)` in production (§3). If sabigoku keeps a clean drain
for debuggability, every row above still applies; the two timeout rows are what
make it safe.

---

## 12. Ownership cheatsheet (Zig → Rust)

| Zig pattern | Rust intent |
|---|---|
| GPA-dupe strings for workers | `String` / `Arc<str>` moved into task |
| Arena-backed history slice | `Vec<Show>` owned by App or `Arc` swap |
| Double-buffered history arenas (reload builds into the idle buffer, flip on success) | build the new `Vec`/`Arc` fully, then swap; never mutate the slice a frame may still borrow |
| `dupeAll` before arming loading | all clones succeed or none; then set loading |
| Worker frees its inputs | task owns params; UI owns events |
| Static provider `name()` | `&'static str` or interned |
| `ThreadDrain` | `AtomicUsize` + `WaitGroup` / drain on `Drop` of runtime guard |

---

## 13. What not to re-import

| zigoku | sabigoku |
|---|---|
| libvaxis `Loop` specifically | ratatui + crossterm + own channel |
| Seven peer bools without a module | OK to group `ResolveSession` / `AsyncHub` |
| Arena free rules as comments-as-safety | types |
| `SOURCE_UNBOUND` episode flag as provider | no-source UI state |
| Join on supersede | never |

---

## 14. Open questions

| ID | Question | Lean |
|---|---|---|
| O1 | ~~Single `tokio` runtime vs pure threads~~ **Closed:** threads + mpsc (M1 cut, ROD-431/433) | closed |
| O2 | Play on UI-blocking thread pool vs dedicated | Dedicated thread per play |
| O3 | Cover decode on worker vs GPU | Worker CPU decode; pixels to UI |
| O4 | ~~Exact discover_cover concurrency default~~ **Closed:** 4, clamped [1,16] (06 §2.2) | — |

---

## 15. Primary sources (freeze)

- `src/tui/app.zig` — `run`, `tick`, drains, bootstrap, quit
- `src/tui/event.zig` — `Event` union
- `src/tui/workers.zig` — tasks, drains, play retry, cover caches
- `src/tui/{episode,cover,discover,discover_covers,prewarm,playback,resolve}_state.zig`
- `src/tui/input.zig`, `render.zig`
- Contracts: **05**; resolve product: **03**; persistence: **02**

---

## 16. Adversarial checklist (ROD-430)

- [ ] UI thread never awaits network/mpv
- [ ] Every detachable worker is drained or generation-safe
- [ ] Superseded episodes/search/cover cannot clobber newer focus
- [ ] Discover feeds write catalog_cache, not library pollution
- [ ] Detail paint has cache-first path
- [ ] Play retry only on open-fail pre-playback
- [ ] Prewarm cancels under fallback
- [ ] Connect cancel does not post into freed state
- [ ] Draw stays pure (no store writes inside render)
