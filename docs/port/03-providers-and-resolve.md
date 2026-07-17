# 03 · Providers and resolve

| Field | Value |
|---|---|
| Status | `draft` |
| Ticket | ROD-424 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Identity | Bindings point **to** AniList shows; see [`02-domain-and-sqlite.md`](02-domain-and-sqlite.md) §1 |
| Nature | **Port design.** zigoku names (Tier 0/A/C, ROD ids) are semantics; storage shapes follow 02. |

---

## 1. Role split

| Layer | Job |
|---|---|
| **AniList** | User-facing catalog: Browse search, Discover, enrichment, sync. Never "search all stream sites." |
| **Stream providers** | Bind a show to a playable catalog id, list episodes, resolve a stream URL. |
| **Resolve orchestration** | Given `anilist_id` (+ optional pin/pref), pick provider, fetch grid, play, fallback, warm. |
| **Player** | Spawn mpv, observe position, optional AniSkip scripts. |

Discovery is not multiprovider search. Provider `search` exists only for **binding**
(tier C) when a site cannot derive an id from the AniList record.

---

## 2. Provider surface (`StreamProvider` / zigoku `SourceProvider`)

App code talks only to this trait. No concrete site types in the TUI.

| Method | Semantics |
|---|---|
| `name()` | Stable persistence key (`"senshi"`, `"megaplay"`, …). Never rename casually. |
| `display_name()` | User-facing (toasts, UI). Free to change. |
| `canonical_key(show) -> Option<provider_id>` | **Tier A:** pure derivation from AniList/MAL metadata (e.g. stringified `mal_id`). `None` means "I do not id-key on canonical," **not** "not stocked." |
| `search(query, opts) -> [hits]` | **Tier C only.** Provider catalog search. Not Browse. |
| `episodes(provider_id, translation, count_hint?) -> [EpisodeLabel]` | Sorted list. **Empty slice = authoritative not stocked** (mark absence). Cannot-answer must **error** (transient). `count_hint` for listing-less providers (mint a 1..N grid); real listings ignore it. |
| `resolve(provider_id, episode, translation, quality) -> StreamLink` | Playable URL + headers/flags. Quality may be ignored if no variants. |
| `cover_request(ref) -> { url, referer?, ua? }` | Turn stored cover ref into an absolute fetch. |

### 2.1 Related types

| Type | Notes |
|---|---|
| `SearchOptions` | `translation`, `limit`, `page` (1-indexed) |
| `search_page_size` | **26** full page (ROD-201). Browse load-more uses AniList, not this, for discovery; provider search pages still use 26 when tier C paginates. |
| `EpisodeLabel` | Free-text raw (`"1"`, `"1.5"`, `"SP1"`). Sort: leading numeric key; non-numeric specials after numbered run. |
| `StreamLink` | `url`, optional `resolution`, `referer`, `user_agent`, `cloaked_segments` (HLS-as-jpg demuxer relax), `sub_url` (external WebVTT) |
| `Quality` | `best` / `1080` / `720` / `480` / `worst`; unknown config → `best` |

### 2.2 Rust shape (intent)

`trait StreamProvider: Send + Sync` (or enum dispatch if the set stays tiny). Registry holds
`&'static dyn StreamProvider` or owned boxed providers. **No** `*anyopaque` vtable theater
(`ZIG-SHAPE`).

---

## 3. Registry

### 3.1 Construction order = default fallback order (freeze)

At freeze (`main.zig` live set, ROD-380 / ROD-343):

1. **megaplay** (default / primary)
2. **senshi**
3. **allanime** (trailing backstop)

Slice is process-immutable. User preference never rewrites the registry in place.

### 3.2 Views

| API | Behavior |
|---|---|
| `primary()` | `providers[0]` (megaplay @ freeze) |
| `by_name(name)` | Owner of a persisted binding key. **Must** use this for bound rows, never `primary()`. `None` = retired provider. |
| `preferred(name)` | Named or `primary()` if empty/unknown |
| `ordered(pref)` | Preferred first, then construction order for the rest (ROD-344) |

### 3.3 When preference applies

**Pin or global preferred_provider** shapes **new** resolution and the fallback walk
snapshot. Paths that already own a concrete binding use `by_name(binding.provider)`.

Under sabigoku identity (02): almost every open is AniList-keyed. Preference + pin
are first-class. zigoku's legacy "provider-keyed `.direct` must not re-route" still
applies when the UI is explicitly on a **chosen binding** (manual flip, pin cycle),
not when inventing a provider for a naked AniList id.

---

## 4. Binding tiers (resolve classifier)

Walk is **tier-major, not provider-major** on first resolve (ROD-343): any existing
binding beats a fresh key on an earlier provider. Within a tier, **effective order**
(`ordered(pref)`) breaks ties.

| Tier | Meaning | Action |
|---|---|---|
| **0 Bound** | `provider_binding` row exists for `(anilist_id, provider)` | Fetch episodes with stored `provider_id` |
| **A Key** | `canonical_key(show)` yields id | Fetch episodes; on success **mint/upsert binding** |
| **B Id match** | Inside tier-C search results: MAL/AniList id agreement (ROD-342) | Prefer over fuzzy title match |
| **C Search** | Title/catalog search + scorer | `best_id_match` then `best_provider_match`; below floor → no bind |

### 4.1 Classifier (`ResolveVerdict`)

Port intent (AniList-first):

```
open(show: AniListId, context):
  if context.explicit_binding:   → Bound { provider, provider_id }   // pin cycle / source flip
  if any binding in effective order: → Bound { first in ordered(pref) }
  if pin set and pin has binding: → Bound { pin's binding }          // also see §6
  for p in ordered(pref):
    if let key = p.canonical_key(show): → TierA { p, key }
  → NeedsSearch { anilist_id }
```

zigoku also had `.direct` for provider-keyed selections where `sel.id` was already a
provider handle. Under 02, that collapses to **Bound** or an explicit binding context.
Do not reintroduce "stringified anilist_id as fake provider id" as a library key.

### 4.2 Tier-C match rules (`resolver.zig` @ freeze)

Pure functions; worker does network search, then scores offline.

**Id match (`best_id_match`) first:**

- AniList id or MAL id agrees with candidate.
- Contradictions veto: episode gap > 3 when total is authoritative, or year gap > 1.
- Corroborated (eps or year agrees) beats bare id; bare still binds if nothing better.

**Fuzzy (`best_provider_match`):**

- Score provider candidates against full AniList record (title + eps + year).
- Thresholds (mirror AniList reverse match): **best ≥ 1200**, **margin ≥ 250**.
- Below either guard: no match (do not bind; treat as miss / absence path as appropriate).

### 4.3 Empty episodes vs error

| Outcome | Meaning |
|---|---|
| `Ok([])` | Authoritative **not stocked** → mark `provider_absence`, try next |
| `Err(_)` | Transient / unknown → do not mark absence; may toast / fallback depending on arm |
| Success non-empty | Clear absence for that provider; upsert binding if pending |

---

## 5. Pins, absences, routes

All keyed by **`anilist_id`** (02). Own tables; enrichment upserts never touch them.

### 5.1 Pin

- At most one provider per show.
- Overrides global preferred for effective preference and History open.
- UI: cycle unpinned → each live provider → unpinned (`v` @ freeze).
- Setting a pin may start a **manual one-provider walk** (probe even through fresh absence).
- Exhausted manual flip: toast like "no match on {name}, pin kept"; pin is not cleared.
- Retired pin name (`by_name` null): do not fetch foreign id on primary (mis-key). Clear or ignore.

### 5.2 Absence (negative cache)

- Row `(anilist_id, provider, checked_at)` = definitive not stocked.
- **TTL @ freeze: 7 days.** Fresh absence ⇒ skip automatic probe/search; **bindings always win.**
- Manual walks (`v` flip, forced preferred tier C) may probe anyway.
- Successful bind **deletes** absence for that pair (bound and absent never coexist).
- Availability UI: `unchecked` | `bound` | `absent` (derived; only absence is stored).

### 5.3 Route stamp (preferred re-route, ROD-398)

Per-show record: `resolved_pref` = the global `preferred_provider` this show last
**settled** under.

| Situation | Behavior |
|---|---|
| Pin set | Route stamp ignored; pin wins |
| Settled pref == live pref and binding exists | Open that binding |
| Settled pref == live pref, no binding | Fall through to normal open of existing state |
| Stale or missing stamp | Force preferred once (tier 0 / A / C on that provider only); **stamp before fetch** so a miss cannot loop forever |
| Empty preferred config | Follow-leader / construction order; route helper no-ops |

**Pin supremacy** and **stamp-before-fetch** are `CLONE` contracts.

---

## 6. Open / play pipelines

### 6.1 Episode grid open (canonical show)

High-level:

1. Clear stale in-flight bind/walk/play-search want (walk hops reinstall their walk).
2. Refresh pin + availability cache for the open `anilist_id`.
3. Maybe enrichment refresh (independent of episode cache).
4. **Preferred re-route** if unpinned and stamp stale (§5.3).
5. Else pin's binding if pin set and bound.
6. Else classifier → bound / tier A / needs search.
7. Episode list cache hit → paint grid; else spawn worker.
8. User-driven open: **do not** arm resume-demote. Only auto-resume landing does.

**Do not join** a prior episode worker on the UI thread. Detach + drain; keep-check drops
stale results (`ZIG-SHAPE` → Rust: generation token / `anilist_id` + cancel flag).

### 6.2 Add to watchlist (`P`)

Same classifier:

- Bound / tier A → probe episodes if needed, mint binding, set user state on `show`.
- Needs search → tier-C add walk (single-flight `add_resolving`).
- No match → miss toast; **never** success without a write.
- Under 02: no `unbound` sentinel row. Library entry is a `show` row; playability is
  separate (bindings optional until resolve succeeds).

### 6.3 Play

1. Resolve stream for current binding + episode + translation + quality.
2. Optional AniSkip prepare (MAL id + episode number).
3. Spawn mpv with referer/UA/cloak/sub flags from `StreamLink`.
4. Observe position; on meaningful progress, write `episode_progress` (02 keys).
5. On `MpvOpenFailed` (exit 2): retry budget with re-resolve; then play-fallback hop.
6. Mid-play prewarm siblings so a source flip is tier-0.

### 6.4 Fallback walk (ROD-346)

After a **failed** episode fetch or stream open, if the show has `anilist_id`:

- Snapshot `ordered(effective_pref)` at walk start (mid-walk pref changes must not reshuffle).
- Mark the failed provider tried; advance to next.
- Per hop: bound id → fetch; else skip fresh absence (unless `manual`); else tier A key;
  else single-provider tier C search.
- One hop per failure event (single-flight with episode/play guards).
- Play continuation: after hop grid lands, **remap episode** (exact raw label, else
  1-based ordinal) and relaunch; walk stays armed (no ping-pong of fresh walks).
- Rescue cancels background prewarm (CDN budget).

**Resume demote (ROD-229):** auto-resume open sets `resume_landing_pending`. Failure
demotes to History list only when the **whole walk** is exhausted, not on intermediate
hops. User-driven opens never arm demote.

### 6.5 Prewarm

After successful add/play: silently try unbound providers (no binding, no fresh absence)
so later flips are tier 0. Once per `anilist_id` per session. Yields to user-facing
resolve and active fallback. Construction order for candidates (pref is not required).

### 6.6 Episode label remap across providers

`map_episode_index(list, raw, ordinal)`:

1. Exact raw string match.
2. Else 1-based ordinal into the sorted list if in range.

Aligns with 02 **L1**: string equality is identity; ordinal is best-effort hop UX, not a
second progress key.

---

## 7. Play and resolve error taxonomy

Map to DESIGN toast matrix (sabigoku DESIGN §4.10). Copy is `CLONE` phrasing unless UX
changes deliberately.

| Class | Example cause | User copy (intent) | Retry / hop |
|---|---|---|---|
| Network down | timeout, refused | `network unreachable` | fallback walk |
| Forbidden | 403 / 451 | `{source} blocked us` | fallback |
| Server error | 5xx | `{source} is down` | fallback |
| Other HTTP | non-200 | `{source} returned an error` | fallback |
| Empty / not stocked | `Ok([])` | miss / absence; hop | mark absence |
| mpv missing | not on PATH | `mpv not found — install mpv` | no hop |
| mpv failed | nonzero exit | `mpv exited with error` | no hop (progress may still count) |
| mpv open failed | exit code 2 | retry toast then hop | re-resolve + play-fallback |
| Episode missing on hop | remap fail | `episode {raw} not found on {source}` | stop play cont |

Watch completion for progress accounting uses **natural end ratio 0.80** (not the 0.95
fully_watched threshold) so a clean quit mid-credits is not a full watch (ROD-168).

---

## 8. Live providers at freeze (enough to reimplement)

Detail protocols stay in provider modules + golden tests. This section is the map.

### 8.1 megaplay (primary)

- **Tier A:** `provider_id = stringified mal_id`. No MAL → no key; **no tier C**
  (`search` unsupported).
- Episodes: listing-less; uses `count_hint` / expected episode count to mint labels.
- Stream: MAL-based embed route; may attach softsub `sub_url`.
- Absence: empty/miss paths mark not stocked when appropriate; unsupported search must
  **not** poison absence for "no mal yet" cases.

### 8.2 senshi

- **Tier A:** stringified `mal_id` (same shape as megaplay).
- Search: POST filter API (tier C recovery when no MAL).
- Episodes / embeds by mal_id; covers via poster CDN.
- HLS may use **cloaked segments** (`cloaked_segments = true` on `StreamLink`).

### 8.3 allanime (backstop)

- GraphQL + persisted query hashes; site facts quarantined in module.
- Stream blob: AES-256-GCM `tobeparsed` (key = sha256(seed); layout prefix/nonce/ct/tag).
  Golden vector in zigoku tests / sabigoku `spike_stream`.
- Has real search + episodes + resolve; trailing in registry order.

### 8.4 Helpers (not full registry members)

| Module | Role |
|---|---|
| `providers/http` | Shared fetch / error class mapping |
| `providers/hls` | Playlist helpers |
| `providers/jikan` | MAL bridge for AniSkip path |
| `resolver` | Tier B/C pure matchers |
| `aniskip` | OP/ED skip times → mpv script; best-effort, never hard-fail play |

### 8.5 Day-one sabigoku set

**Lean (locked for draft):** implement the same three names/order unless a later decision
drops allanime. Protocol work can phase, but the **registry + tier machinery** ships
with multiprovider in mind from day one (DESIGN multiprovider).

`OPEN`: exact ship order of allanime vs megaplay/senshi only if rate-limit/legal risk
forces a stub; default = parity with freeze lineup.

---

## 9. AniSkip (play adjunct)

- Keyed on **MAL id** + numeric episode (from label or ordinal).
- Modes from config: op / ed / both; unknown string → both (typo must not disable).
- Fetch skip times; write `skip.lua` + mpv opts; announce before seek.
- Any failure → plain play, no error toast.

---

## 10. In-flight transport (product rules)

| Flag / concept | Rule |
|---|---|
| `add_resolving` | One tier-A/C **add** at a time (mashed P must not fan CDN) |
| `play_resolving` | Tier-C **play** search; independent of add |
| `play_resolve_aid` | Staleness gate: late result ignored if nav left that show |
| `pending_bind` | Anilist id to mint when episodes succeed |
| `fallback` | Active walk; hops park/reinstall across grid fire |
| Episode generation | Superseded fetch must not clear live load or toast |

Rust: prefer explicit `ResolveSession` / generation counters over seven peer bools on
`App` (`ZIG-SHAPE` cleanup OK if semantics match).

---

## 11. What not to port from zigoku resolve

| zigoku artifact | sabigoku |
|---|---|
| `SOURCE_UNBOUND` pseudo provider | Show without bindings (02) |
| `anime` PK `(source, source_id)` as resolve identity | `anilist_id` + `provider_binding` |
| `isCanonicalKeyed` stringified-id hack | Real AniList id on show |
| COALESCE join for enrichment mid-resolve | `show` / `catalog_cache` reads |
| Arena dupe / exact-fit free rules as product | Intent only: whole-buffer ownership across threads |

---

## 12. Store capabilities this chapter needs (from 02)

- get/set pin, absence (fresh?), route stamp
- list/get binding by `(anilist_id, provider)` and reverse `(provider, provider_id)`
- upsert binding on successful episodes
- episode list cache get/set
- episode progress get for resume start seconds
- catalog_cache / show enrichment for match inputs (`mal_id`, titles, year, totals)

---

## 13. Primary sources (freeze)

- `src/source.zig` — trait, registry, page size
- `src/tui/resolve.zig`, `resolve_state.zig` — pipelines, fallback, preferred route
- `src/tui/app.zig` — `ResolveVerdict`, `Fallback`, failure copy, drains
- `src/resolver.zig` — tier B/C matchers
- `src/providers/{megaplay,senshi,allanime,http,hls,jikan}.zig`
- `src/player.zig`, `src/aniskip.zig`
- `src/main.zig` — live registry order
- DESIGN toast matrix §4.10

---

## 14. Open questions

| ID | Question | Lean |
|---|---|---|
| O1 | Ship all three providers day one vs phase allanime | Same three; phase only if forced |
| O2 | Enum dispatch vs `dyn StreamProvider` | Enum fine while N≤3–4 |
| O3 | Absence TTL keep 7d | `CLONE` until measured otherwise |

---

## 15. Adversarial checklist (ROD-430)

- [ ] Tier order is binding-first, then key, then search (not provider-major on first open)
- [ ] Empty episodes ⇒ absence; errors do not
- [ ] Pin overrides preferred; route stamp does not fight pin
- [ ] Stale preferred re-route stamps before fetch (no loop)
- [ ] Resume demote only on auto-resume walk exhaust
- [ ] No `unbound` / provider-primary library identity required
- [ ] Tier-C thresholds and id-match vetoes named
- [ ] Play error classes map to user copy + hop/retry policy
- [ ] Episode remap uses string equality then ordinal (02 L1)
