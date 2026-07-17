# 01 · Modules and data flow

| Field | Value |
|---|---|
| Status | `review-stable` |
| Ticket | ROD-422 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Depends on | [02](02-domain-and-sqlite.md) · [03](03-providers-and-resolve.md) · [04](04-tui-runtime.md) · [05](05-behavior-contracts.md) · [DESIGN.md](../../DESIGN.md) |
| Nature | **Map only.** No layout essays; no protocol dumps. |

---

## 1. Purpose

Name the boxes, who owns what, and the direction of calls so M1 does not invent a
second architecture. Implementation language is Rust; zigoku paths are **evidence**.

---

## 2. Process shape

```
sabigoku [args]
    │
    ├─ login          → auth / OAuth loopback (06)
    ├─ sync           → pull then push AniList (06)
    ├─ update         → self-update / check (06)
    └─ (default) TUI  → open store → registry → tui::run
              optional: one-shot CLI search/play (zigoku had non-TUI path; sabigoku may defer)
```

**Bootstrap (TUI):** resolve paths → load config → open SQLite (migrate) → build
provider registry → construct App → event loop (04).

**Independent store:** sabigoku DB only (02 L3). No zigoku file open.

---

## 3. Logical modules

Suggested Rust layout (names indicative; merge small crates if noise wins). Arrows
mean "may depend on" downward.

```
                    ┌─────────────┐
                    │  bin/main   │
                    └──────┬──────┘
           login/sync/tui  │
        ┌──────────────────┼──────────────────┐
        ▼                  ▼                  ▼
   ┌─────────┐      ┌────────────┐     ┌────────────┐
   │  auth   │      │   sync     │     │    tui     │
   └────┬────┘      └─────┬──────┘     └─────┬──────┘
        │                 │                  │ workers = the glue point:
        │                 ▼                  │ imports source, store, player,
        │           ┌──────────┐             │ resolver, anilist
        └──────────►│ anilist  │◄────────────┤
                    └────┬─────┘             │
                         │           ┌───────▼──────┐
                    ┌────▼─────┐     │   resolve    │
                    │  domain  │     │   (orch.)    │
                    └──────────┘     └──────────────┘
   ┌─────────┐  ┌───────────┐  ┌───────────┐  ┌──────────┐  ┌──────────┐
   │ config  │  │   store   │  │ providers │  │ resolver │  │  player  │
   │ paths   │  │           │  │ registry  │  │ (pure)   │  │ aniskip  │
   └─────────┘  └───────────┘  └───────────┘  └──────────┘  └──────────┘
```

Verified import edges at freeze (importer → imported); the diagram is a sketch,
this table is law:

| Module | Imports |
|---|---|
| `domain` | nothing (pure) |
| `source` | domain |
| `providers/*` | source (+ own http/hls helpers); **never** tui/store |
| `store` | domain, paths **only** (no source, no providers) |
| `player` | domain, paths **only** (StreamLink in, mpv out; glued in tui/workers) |
| `resolver` | domain, **anilist** (reuses its pure scorers; see §5) |
| `anilist` | domain, source, util |
| `auth` | paths |
| `sync` | anilist, auth, domain, store |
| `aniskip` | providers/jikan, player, paths |
| `config` | domain, paths |
| `tui/workers` | source, store, player, resolver, anilist (the one glue point) |

| Module | Responsibility | Deep doc |
|---|---|---|
| **domain** | Show, binding DTOs, ListStatus, Translation, Quality, EpisodeLabel, StreamLink, title helpers, after_play / is_still_airing | 02 §4 |
| **store** | SQLite: `show`, `catalog_cache`, bindings, progress, pins/absences/routes, migrate, history queries | 02 |
| **source / providers** | `StreamProvider` trait, registry order, megaplay/senshi/allanime (+ http/hls helpers) | 03 |
| **resolver** | Pure tier-B/C matchers (id + fuzzy) | 03 §4.2 |
| **anilist** | GraphQL search, discover axes, enrich, list push/pull | 06 + DESIGN data reality |
| **auth** | Token file, parse, expiry | 06 |
| **login / loopback** | OAuth browser + local callback | 06 |
| **sync** | pull-then-push, reconcile, dirty set | 06 |
| **config / paths** | User prefs, preferred_provider, palette, mpv_path, DB path | 06 |
| **player** | mpv spawn, IPC position, StreamLink flags | 03 §7 |
| **aniskip** | Skip times → mpv script | 03 §9 |
| **resolve (orch.)** | Tiers, pin/pref/route, fallback walk, prewarm fire, episode/play fire | 03, 05 §10 |
| **tui** | App state, input, render, workers, event loop | 04, 05, DESIGN |
| **cover** | Fetch/decode pixels, caches | 04 §7.3–7.4 |
| **update / updatecheck** | Release check / apply | 06 |
| **log** | Structured debug/err | — |

### 3.1 TUI internal slices

Keep transport off pure view math where zigoku already carved modules:

| Slice | Owns |
|---|---|
| `event` | `Event` enum + channel alias |
| `workers` | spawn tasks, drains, dupe helpers |
| `resolve` + `resolve_state` | orchestration free functions + in-flight flags |
| `episode_state` | grid + LRU + cursor/progress |
| `discover_state` / `discover_covers` | axes + URL cover pump |
| `cover_state` | detail cover decision + pixels |
| `prewarm_state` | warm walk guard/cancel |
| `playback_session` | checkpoint / finish vs store |
| `search_state` / `settings_state` / `selection` | records |
| `input` | key dispatch by mode/view |
| `render` + theme/colors | pure draw from App + DESIGN tokens |

---

## 4. End-to-end data flows

### 4.1 Cold start → History

```
main → paths/config → store.open+migrate → registry::live()
    → tui::run
         → worker: loadHistory → HistoryLoaded
         → optional: launch pull (sync) → SyncFlushed → maybe reload
         → optional: resume landing (05) → resolve open (03) → episodes
```

### 4.2 Browse search → detail → play

```
key '/' → search mode → debounced worker → AniList search
    → SearchDone → results in memory
    → upsert catalog_cache (02)
    → focus card → detail paint from catalog_cache (cache-first)
    → Enter/P → resolve classifier (03)
         Tier0 binding | TierA key | TierC search+match
    → episodes worker → grid
    → play worker → mpv → PositionUpdate / PlayDone
    → store: episode_progress + show user state
```

### 4.3 Discover feed

```
enter Discover → per-axis slot cache hit? else discoverFeedTask
    → DiscoverFeed → slot append + catalog_cache upsert
    → cover pump (URL workers)
    → Enter → same resolve path as Browse (anilist_id)
```

### 4.4 Preferred re-route / fallback

```
open show (anilist_id)
    → pin? → that binding
    → else route stamp vs preferred (03 §5.3)
    → else classifier + ordered(pref)
on fail → FallbackWalk hop (bound → A → C) → demote only if resume-armed (05)
```

### 4.5 CLI sync (non-TUI)

```
sabigoku sync → open store → auth credentials
    → pullAll (first) → pushAll
    → print summary; never push-first on first contact (06)
```

---

## 5. Dependency rules (footguns)

| Rule | Why |
|---|---|
| **providers** must not import **tui** | Keep play backends testable offline |
| **store** must not import **tui** or **providers** (except maybe name strings) | Persistence is policy-free of HTTP |
| **domain** has no I/O | Pure |
| **resolver** pure; no network | Worker searches, then scores. zigoku's resolver imports `anilist` for its pure scoring helpers even though it never calls the network functions; in Rust either hoist the scorer into a shared module or consciously accept the HTTP-capable dep (M1 decision) |
| **tui/render** should not write store | Draw pure (04); mutations in tick/handlers |
| **anilist** is the only user-facing catalog client | Providers do not power Discover/Browse search |
| Enrichment upserts never touch pins/absences/routes | 02/03 |

---

## 6. Ownership boundaries (runtime)

| Owner | Data |
|---|---|
| UI thread | All App/view state; sole store writer from TUI path (or serialized store handle) |
| Workers | Temporary arenas/allocs; post **owned** events; no shared mutable App |
| Store connection | Single writer preferred; WAL + busy timeout if multi (02 concurrent open) |
| Registry | Process-immutable provider list; pref is a **view** (03) |

SQLite from workers: either (a) only UI thread touches store and workers return data,
or (b) `Mutex<Connection>` / pooled handle with clear rules. zigoku mixed UI-thread
writes with care; sabigoku should **pick one** in M1 and document in 08 if needed.
**Lean:** UI thread owns store writes; workers return values in events (simpler
reasoning, matches most tick handlers).

---

## 7. Zigoku → sabigoku file map (orientation)

| zigoku | sabigoku target |
|---|---|
| `main.zig` | `src/main.rs` + cli module |
| `domain.zig` | `domain` |
| `store.zig` | `store` (new schema 02) |
| `source.zig` + `providers/*` | `providers` |
| `resolver.zig` | `resolver` |
| `anilist.zig` | `anilist` |
| `tui/app.zig` + slices | `tui::*` |
| `tui/resolve.zig` | `tui::resolve` or `resolve` crate used by tui |
| `tui/workers.zig` | `tui::workers` |
| `player.zig` / `aniskip.zig` | `player` / `aniskip` |
| `config.zig` / `paths.zig` / `auth.zig` | `config` / `paths` / `auth` |
| `sync.zig` / `login*.zig` | `sync` / `login` |
| `provider_migrate.zig` | **drop** (zigoku-only ladder); sabigoku migrations live in store |

---

## 8. Milestone wiring (not a schedule)

| Milestone | Modules first |
|---|---|
| M1 | domain, store, providers trait+one provider, thin resolve, spike collapse |
| M2 | tui shell + history + catalog_cache read/write |
| M3 | full resolve walk, play, multiprovider |
| M4 | Discover, covers, settings polish |
| M5 | auth, sync, update |

Contracts (**05**) gate behavior at each step; DESIGN gates pixels.

---

## 9. Open questions

| ID | Question | Lean |
|---|---|---|
| O1 | Workspace crates vs single package modules | Single package + `mod` until compile times hurt |
| O2 | Store access from workers | UI-thread writes only (§6) |
| O3 | Keep non-TUI CLI play path | Defer; TUI is the product |

---

## 10. Adversarial checklist (ROD-430)

- [ ] AniList is only discovery catalog client on the diagram
- [ ] Bindings hang off show, not the reverse
- [ ] catalog_cache appears on search/discover write path
- [ ] No arrow from providers → tui
- [ ] Resolve orchestration distinct from provider HTTP
- [ ] CLI sync is pull-then-push
- [ ] Independent store called out (no zigoku import module)
