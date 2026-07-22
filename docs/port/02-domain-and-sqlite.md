# 02 · Domain and SQLite

| Field | Value |
|---|---|
| Status | `review-stable` |
| Ticket | ROD-423 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Nature | **Port design**, not a schema transcript. Zigoku is evidence and scar tissue. |

---

## 1. Port decision: identity

**AniList id is the source of truth for show identity.**

A show in sabigoku *is* an AniList media id. Catalog, metadata, watchlist membership,
list status, progress counters, pins, absences, and routes hang off that id.

When a streaming provider has an offering for that show, we store a **binding**:
`(anilist_id, provider, provider_id)`. The binding points **at** the AniList show.
The show does not exist "as" a provider row that later grows an optional AniList
column.

```
┌─────────────────────────────┐
│  show (PK = anilist_id)     │  identity + enrichment + user state
└──────────────┬──────────────┘
               │ 1:N
               ▼
┌─────────────────────────────┐
│  provider_binding           │  how to play it on a given source
│  (anilist_id, provider,     │
│   provider_id)              │
└─────────────────────────────┘
```

This inverts zigoku's end state, where the primary row was a provider handle and
canonical identity was bolted on later. See §2 for how that happened and why we
are not carrying it forward.

**Disposition:** `FIX-IN-RUST` for the identity model. Behavioral contracts that
assume "one history card per show" still hold; the storage that makes them true
changes.

---

## 2. What zigoku did (archaeology, not blueprint)

Read this so implementers do not re-derive the same trap. Do not re-implement
this ladder.

### 2.1 Original intent vs first schema

Product intent was always AniList-shaped (catalog, enrichment, sync). The **v1
schema** still keyed the library table on the play handle:

```
anime PRIMARY KEY (source, source_id)
anilist_id / mal_id  -- nullable enrichment columns
```

zigoku's own comment (`store.zig` header @ freeze) justifies that as: playable
identity is the provider handle; enrichment ids arrive later, so they cannot be
the PK; and provider id namespaces must not collide across sources.

That was true for a **single-provider, play-first** app. It stopped being the
right spine once AniList became catalog + multiprovider + sync.

### 2.2 Enrichment accreted on the binding row (v2–v11)

Columns piled onto `anime`: year/status/description/score, `history_visible`
(v3–v4: hide pure search-cache pollution), season/native/genres, enrichment
freshness + fieldset version, studios/duration/rank/airing/country, AniList sync
snapshots (`synced_status` / `synced_progress`).

User state and catalog metadata shared the provider-keyed row. Search hits that
were never "really" library entries polluted the same table until
`history_visible` papered over it.

### 2.3 Provider re-key as identity crisis (v12)

`allanime` → `senshi` re-key via stringified `mal_id` (ROD-304). Because identity
*was* `(source, source_id)`, changing provider meant rewriting PKs and cascading
children under `defer_foreign_keys`. That is the cost of provider-primary keys
when the real continuity is the show.

### 2.4 Canonical bolted on (v14+)

Multiprovider forced a second spine:

- `canonical_anime` PK = `anilist_id` (identity + enrichment only, by invariant)
- `anime.canonical_id` nullable FK (link from binding → canonical)
- Later: `provider_pins`, `provider_absences`, `provider_routes` keyed by
  `canonical_id` (v15–v18), deliberately **not** on the enrichment upsert path

Correct direction, wrong layering order. The app still:

- treats `(source, source_id)` as the library PK
- COALESCE-joins enrichment from `canonical_anime` at read time
- mints `source = 'unbound'` pseudo-bindings when a show has user state but no
  play provider (ROD-329)
- re-keys / supersedes / unions across sibling bindings so History looks like one
  card per show

So the mental model became AniList-first while the **schema remained
provider-first with a canonical sidecar**. Every multiprovider feature paid a tax
for that gap (V17 anipub retirement is the clearest bloodstain: re-key orphans to
`unbound` so History does not go dark).

### 2.5 End state at freeze (`SCHEMA_VERSION = 18`)

| Table | Role in zigoku |
|---|---|
| `anime` | PK `(source, source_id)`: binding + duplicated enrichment + **user state** |
| `episode_progress` | PK `(source, source_id, translation, episode)` FK → anime |
| `episode_cache` | PK `(source, source_id, translation)` FK → anime |
| `canonical_anime` | PK `anilist_id`: enrichment spine |
| `provider_pins` / `absences` / `routes` | per-canonical multiprovider policy |
| `app_meta` | one-shot flags outside the version ladder |

---

## 3. Target model (sabigoku)

### 3.1 Principles

1. **One show row per AniList id.** No sibling binding rows competing to own
   `list_status` / `progress` / `history_visible`.
2. **Bindings are edges, not the node.** Play, episode lists, and provider-opaque
   ids live on binding (and binding-scoped caches).
3. **User state never lives on a provider id.** Switching preferred source or
   pinning must not fork or drop watch state (the V12/V17 class of bug).
4. **No play provider ⇒ no binding row**, not a fake `unbound` source. Playability
   is "has at least one binding" or "resolve can find one." History membership is
   its own explicit marker, not an accident of row existence: see §3.7.
5. **Catalog cache is separate from the library.** Browse/Discover AniList hits
   land in `catalog_cache` (durable, no user state). They never share a table with
   watchlist rows and never need `history_visible` laundering. See §3.5.
6. **Pins, absences, routes stay off the enrichment upsert path** (keep zigoku's
   table-split invariant; it was right).
7. **Independent store.** sabigoku does not open, migrate, or import zigoku DBs.
   No shared path, no compatibility ladder, no dual-read. A zigoku importer is a
   **separate future issue**, designed later if needed.

### 3.2 Logical entities

| Entity | Key | Owns |
|---|---|---|
| **Show** | `anilist_id` | Library: titles/enrichment + list status, progress, ratings/notes, sync snapshots, enrichment freshness |
| **CatalogCache** | `anilist_id` | Durable AniList search/Discover hit metadata for card + detail populate; **no** user state |
| **ProviderBinding** | `(anilist_id, provider)` unique; `provider_id` opaque | How to talk to a stream source for this show |
| **EpisodeProgress** | see §3.4 | Resume + fully_watched per episode label per translation |
| **EpisodeCache** | `(anilist_id, provider, translation)` | Provider episode label lists + TTL |
| **ProviderPin** | `anilist_id` | Forced provider for resolve |
| **ProviderAbsence** | `(anilist_id, provider)` | Negative cache "not stocked" + checked_at |
| **ProviderRoute** | `anilist_id` | Last settled preferred_provider (stale ⇒ re-route once; ROD-398 intent) |
| **AppMeta** | `key` | One-shot flags |

`mal_id` is a **secondary** index on Show (AniSkip, legacy bridges), never the PK.
Non-unique in the wild; do not pretend otherwise.

### 3.3 Draft table sketch

Names are indicative. M1 may rename; the keys and ownership must hold.

```sql
-- Identity + catalog + user state (one row per show)
CREATE TABLE show (
    anilist_id                  INTEGER PRIMARY KEY,
    mal_id                      INTEGER,
    -- titles / enrichment (fieldset versioned; see §4)
    title_romaji                TEXT NOT NULL,
    title_english               TEXT,
    title_native                TEXT,
    cover_url                   TEXT,
    total_episodes              INTEGER,
    duration_minutes            INTEGER,
    year                        INTEGER,
    season                      TEXT,
    status                      TEXT,          -- AniList media status
    description                 TEXT,
    score                       INTEGER,
    kind                        TEXT,
    start_year                  INTEGER,
    start_month                 INTEGER,
    start_day                   INTEGER,
    genres                      TEXT,          -- JSON string array (L5); display list
    studios                     TEXT,
    source_material             TEXT,
    rank                        INTEGER,
    rank_type                   TEXT,
    rank_year                   INTEGER,
    next_airing_at              INTEGER,
    next_airing_episode         INTEGER,
    country                     TEXT,
    enrichment_fetched_at       INTEGER,
    enrichment_fieldset_version INTEGER,
    -- user state
    list_status                 TEXT NOT NULL DEFAULT 'planning',
    user_rating                 REAL,
    notes                       TEXT,
    play_count                  INTEGER NOT NULL DEFAULT 0,
    progress                    INTEGER NOT NULL DEFAULT 0,
    -- NULL = identity row only (bindable, probeable), NOT in the library (§3.7)
    library_added_at            INTEGER,
    last_watched_at             INTEGER,
    -- AniList list sync snapshots (dirty = live pair differs or NULL)
    synced_status               TEXT,
    synced_progress             INTEGER
);

CREATE INDEX idx_show_mal ON show(mal_id);
CREATE INDEX idx_show_list_status ON show(list_status);
CREATE INDEX idx_show_last_watched ON show(last_watched_at DESC);

CREATE TABLE provider_binding (
    anilist_id   INTEGER NOT NULL REFERENCES show(anilist_id) ON DELETE CASCADE,
    provider     TEXT    NOT NULL,
    provider_id  TEXT    NOT NULL,
    bound_at     INTEGER NOT NULL,
    PRIMARY KEY (anilist_id, provider),
    UNIQUE (provider, provider_id)
);

CREATE TABLE episode_progress (
    anilist_id     INTEGER NOT NULL REFERENCES show(anilist_id) ON DELETE CASCADE,
    translation    TEXT    NOT NULL,  -- sub | dub
    episode        TEXT    NOT NULL,  -- raw label: "1", "1.5", "SP1"
    position_secs  REAL    NOT NULL DEFAULT 0,
    duration_secs  REAL    NOT NULL DEFAULT 0,
    fully_watched  INTEGER NOT NULL DEFAULT 0,
    updated_at     INTEGER NOT NULL,
    -- optional audit: which provider last wrote this row
    last_provider  TEXT,
    PRIMARY KEY (anilist_id, translation, episode)
);

CREATE TABLE episode_cache (
    anilist_id   INTEGER NOT NULL REFERENCES show(anilist_id) ON DELETE CASCADE,
    provider     TEXT    NOT NULL,
    translation  TEXT    NOT NULL,
    episodes_blob TEXT   NOT NULL,
    fetched_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    PRIMARY KEY (anilist_id, provider, translation)
);

CREATE TABLE provider_pin (
    anilist_id INTEGER PRIMARY KEY REFERENCES show(anilist_id) ON DELETE CASCADE,
    provider   TEXT NOT NULL
);

CREATE TABLE provider_absence (
    anilist_id INTEGER NOT NULL REFERENCES show(anilist_id) ON DELETE CASCADE,
    provider   TEXT    NOT NULL,
    checked_at INTEGER NOT NULL,
    PRIMARY KEY (anilist_id, provider)
);

CREATE TABLE provider_route (
    anilist_id     INTEGER PRIMARY KEY REFERENCES show(anilist_id) ON DELETE CASCADE,
    resolved_pref  TEXT NOT NULL
);

CREATE TABLE catalog_cache (
    anilist_id                  INTEGER PRIMARY KEY,
    mal_id                      INTEGER,
    -- metadata returned by search / Discover / page queries (fieldset-versioned)
    title_romaji                TEXT NOT NULL,
    title_english               TEXT,
    title_native                TEXT,
    cover_url                   TEXT,
    total_episodes              INTEGER,
    duration_minutes            INTEGER,
    year                        INTEGER,
    season                      TEXT,
    status                      TEXT,
    description                 TEXT,
    score                       INTEGER,
    kind                        TEXT,
    start_year                  INTEGER,
    start_month                 INTEGER,
    start_day                   INTEGER,
    genres                      TEXT,
    studios                     TEXT,
    source_material             TEXT,
    rank                        INTEGER,
    rank_type                   TEXT,
    rank_year                   INTEGER,
    next_airing_at              INTEGER,
    next_airing_episode         INTEGER,
    country                     TEXT,
    fieldset_version            INTEGER NOT NULL,
    fetched_at                  INTEGER NOT NULL,
    -- optional soft expiry for background refresh; reads may still serve stale
    expires_at                  INTEGER
);

CREATE INDEX idx_catalog_fetched ON catalog_cache(fetched_at DESC);

CREATE TABLE app_meta (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
);
```

`catalog_cache` column set tracks the AniList **card + detail** enrichment
surface (same fieldset idea as library `show`, without user-state columns). Exact
column parity with `show`'s enrichment half is intentional so promote-to-library
is a straight copy into `show` plus defaults for user fields.

### 3.4 Episode progress keying (**locked**)

**Locked:** progress rows key by **`(anilist_id, translation, episode_label)`**,
not by provider. Watch state belongs to the show.

**Locked equality rule:** **same episode string ⇒ same episode.** `"1"` is `"1"`
across providers. No automatic remap, no integer-normalization layer in v1.

**Mismatch handling:** out of band for the store. If two providers use different
labels for what a human considers the same episode (or the same label for
different ones), that is a **future UX task**: human-in-the-loop decision, not
schema gymnastics. Track in [`07-bug-ledger.md`](07-bug-ledger.md) as known
product risk + planned UX; do **not** put user state back on
`(provider, provider_id)` to paper over it.

`episode` stays **TEXT**, never INTEGER. Labels are not always integers (`1.5`,
`SP1`). That zigoku choice is `CLONE`.

### 3.5 Catalog cache (**locked**) vs library `show`

AniList search and Discover queries return **limited but useful** metadata. Opening
a detail card must not force a full network re-fetch every time. **Locked:** a
durable **`catalog_cache`** table holds those hits.

| Concern | Rule |
|---|---|
| What goes in | AniList-keyed enrichment from Browse search, Discover feeds, and similar list/page queries |
| What does **not** go in | `list_status`, progress, ratings, notes, sync snapshots, pins, bindings |
| Read path | Detail card / preview prefers `catalog_cache` by `anilist_id`; network only on miss or explicit refresh / expiry policy |
| Write path | Upsert on every successful AniList list/search page (and fuller enrich when we already paid for it) |
| Promote to library | User add / successful `recordPlay` / plan copies enrichment into `show` and stamps `library_added_at` (§3.7). Cache row may remain |
| Stale data | `fieldset_version` + `fetched_at` / `expires_at`. Stale may still paint the card; background refresh is a runtime concern (04), not a reason to skip durability |
| Library shows | Detail/preview for a **library** show renders from `show`; `catalog_cache` serves non-library cards only. No read-time merge of the two (that is the COALESCE dual-spine reborn). Feed/search upserts always write `catalog_cache` and also patch `show` enrichment when the show is library |
| vs zigoku | Replaces "upsert into `anime` with `history_visible = 0`" without polluting the library |

| zigoku | sabigoku |
|---|---|
| Search hit → `anime` + `history_visible = 0` | Search hit → `catalog_cache` only |
| `source = 'unbound'` sentinel binding | `show` without bindings; no fake provider |
| COALESCE binding ↔ canonical on History load | History = `SELECT … FROM show` (+ binding aggregate for sources) |

### 3.6 Independent store (**locked**)

**Locked:** sabigoku's SQLite file is **totally independent** of zigoku's.

- New schema, own `user_version` ladder starting at 1 for the §3 shape.
- No code path opens a zigoku DB, reads `user_version = 18`, or dual-stacks migrations.
- No importer designed in this bible chapter or in M1 store work.
- A "import from zigoku" feature, if ever wanted, is its **own Plane issue at a
  later date**, with its own design. Until then: greenfield only.

zigoku remains a **behavior and archaeology reference** (freeze rev), not a data
dependency.

### 3.7 Library membership (**locked**)

Round 1 of the adversarial review (ROD-430) found that FK-parenting every policy
table on `show` while equating "show row" with "History row" rebuilds zigoku's
search-pollution problem by construction: tier-A resolve mints a binding on
episode success, a binding needs a `show` row, and suddenly opening an episode
grid adds a History entry. The fix is an explicit marker column:

| Rule | One sentence |
|---|---|
| Show row creation | A `show` row is minted by whichever comes first: binding mint, absence mark, route stamp, or library add. Identity rows are cheap and carry no UI meaning. (Route stamp joined the list at ROD-439: stamp-before-fetch must land for never-resolved shows, 03 §5.3.) |
| History contents | History = `show` rows with `library_added_at IS NOT NULL`. Nothing else, ever. |
| Membership set by | Watchlist add (`P` / reveal), user status writers (`setListStatus` / restore), or a **successful `recordPlay`**. Stamp membership **inside** those writers (set-once). Callers gate `recordPlay` with meaningful position (finite pos > 0) + known episode index; do not re-derive membership from isMeaningful alone. Partial watches join History. |
| Membership NOT set by | Episode grid open, availability probe, prewarm, enrichment, Discover/Browse paint, sync `applyPulled`, bind-on-resolve with `visible=false`. Natural-end / `completed` (progress ratchet, §4b) is **not** the membership gate. |

Ancestry: zigoku's `history_visible` was a `MAX()` ratchet folded into the upsert
(store.zig:633 @ freeze), which is why search pollution had to be laundered after
the fact. `recordPlay` always sets visible when it runs; `completed` only
ratchets progress (playback_session.zig @ freeze: "partial watches appear in
history"). `library_added_at` keeps that engagement ratchet (set once;
enrichment can never unset it) as a first-class column instead of laundering.

---

## 4. Domain types (logical)

Language-shaped names in Rust later (`Show`, `ListStatus`, …). Semantics `CLONE`
from zigoku `domain.zig` unless noted.

| Type | Semantics |
|---|---|
| `Translation` | `sub` \| `dub` |
| `ListStatus` | `planning`, `watching`, `paused`, `completed`, `dropped` |
| `ListStatus::after_play` | completed sticks; still-airing never auto-completes; else complete when `progress >= total`; else watching (ROD-139, ROD-296) |
| `is_still_airing(status)` | denylist: only `FINISHED` / `CANCELLED` settled; null/unknown/RELEASING/HIATUS/… = airing |
| `Quality` | stream quality preference enum (see domain @ freeze) |
| `Season` / `Cour` / `Date` | enrichment calendar helpers |
| `TitleLanguage` | `romaji` \| `english` \| `native` with fallback chain (DESIGN + ROD-205) |
| `EpisodeNumber` | raw label string + helpers |
| `StreamLink` | playable URL + metadata for mpv |
| `max_episode_hint` | cap untrusted grid allocs (10_000 @ freeze) |
| `expected_episode_count` | (ROD-359) airing with `next_airing_episode` present: `aired = next_airing_episode - 1`, except `next_airing_episode <= 1` → **null** (nothing aired yet; null ≠ 0), and `min(aired, total)` when total is known. Airing **without** `next_airing_episode`: falls back to `total`. Settled: `total`. `max_episode_hint` clamps every branch |
| Resume ratios | `WATCHED_RATIO = 0.95`, `NATURAL_END_RATIO = 0.80` (`CLONE`); interplay table in §4b |
| `score` vs `user_rating` | `score` = AniList community `averageScore`, 0–100 integer, enrichment-side. `user_rating` = the user's own 0–10, user-state-side. Never conflate |

**Show vs binding in the type model**

- Catalog/search DTOs from AniList carry `anilist_id` as required (not optional
  enrichment).
- Provider search (tier-C binding) returns provider ids that are **candidates to
  link**, not library identity.
- In-memory TUI "focused show" should be `anilist_id`-keyed; active binding is a
  field on that focus, not the focus key itself.

---

## 4b. Progress arithmetic (law)

The single `show.progress` integer feeds History display and the number pushed to
AniList. Exact rules (zigoku `store.zig` @ freeze; ROD-193/296/346 tests):

| Writer | Rule |
|---|---|
| `recordPlay(completed=true)` | `progress = max(progress, episode_index)`: a ratchet, never lowers. `episode_index` is the 1-based ordinal in the current grid |
| `recordPlay` (callers gate on a **meaningful** position: finite > 0) | Always bumps `play_count` and `last_watched_at`, and stamps `library_added_at` if null (set-once, inside the writer, §3.7); a rewatch still counts as engagement (History sort) |
| `setListStatus(completed)` | Snaps `progress = total` iff `total > 0` (total 0 leaves progress alone) |
| Undo (`restoreListStatus`) | Restores the exact prior `(status, progress)` pair; no snap |
| Recompute (`r`) | Positional high-water (below); overwrites unconditionally |
| Reroute / landing joins | Raise-only variant `MAX(progress, union)` so a force-completed sibling never un-completes (ROD-346) |

**Recompute contract** (zigoku comment, verbatim intent: "1-based ordinal of last
fully-watched among present rows (sortKey order), not a count and not absolute ep
number. Gap-watch under-counts on purpose."):

1. Take this show's `episode_progress` rows for the **active translation only**.
2. Sort by numeric-prefix key; non-numeric labels (`SP1`, `OVA`) sort last.
3. `progress` = 1-based position of the **last `fully_watched` row** in that
   sorted set. Positional: not the label value, not a count. Only ep "5" watched
   → progress 1. No rows → 0.

zigoku unions rows across sibling bindings before sorting; the §3.4 keying
(`anilist_id`, not provider) makes that union automatic.

**Translation:** progress rows are per-translation; the one `show.progress`
integer is translation-blind (whichever translation last completed a play
ratchets it). Resume and counting never mix sub with dub.

**Resume marker:** derived UI state, not a column. The episode grid's resume seed
returns null when `progress == 0`; that is all "recompute-to-0 clears the marker"
means. `episode_progress.position_secs` rows are untouched by recompute.
(sabigoku deviation: the marker derives from `latest_resume` gated by
`show.progress_stamped_at`, so any frontier move retires older partials while the
rows still remain; 08 §10 ROD-477.)

**Clamping:** storage is **unclamped**. `progress` may exceed `total` (overshoot
still counts as completed). The "14/2" fix (0.3.1 / ROD-297) is a render-time
clamp only; clamping on write makes stored watch history lossy.

**Threshold interplay** (single authority; 03 §7 and 05 §11 cite this table):

| position / duration | recordPlay | progress ratchet, cursor advance, dim | fully_watched |
|---|---|---|---|
| < 0.80 | yes | no | no |
| ≥ 0.80 (natural end) | yes | yes | no |
| ≥ 0.95 | yes | yes | yes |

---

## 5. Behaviors to preserve (storage-level)

These are product invariants; only the tables they sit on change.

| Invariant | zigoku evidence | sabigoku note |
|---|---|---|
| Concurrent open migrates atomically | `BEGIN IMMEDIATE` whole ladder; busy timeout; re-read version under lock (ROD-287) | `CLONE` mechanism |
| Schema too new ⇒ hard error | `SchemaTooNew` | `CLONE` |
| Single `user_version` ladder, one bump at end | migrate() @ freeze | `CLONE` |
| Enrichment fieldset version heals columns without full TTL | `ENRICHMENT_FIELDSET_VERSION = 5` | `CLONE` idea; renumber for our column set |
| Upsert must not clobber user state with search/enrichment | user-state columns are **excluded from the `ON CONFLICT … DO UPDATE SET` clause entirely** (`list_status`, `user_rating`, `notes`, `play_count`, `progress`, `library_added_at`, `last_watched_at`); COALESCE guards only nullable enrichment columns | **Not COALESCE.** `COALESCE(excluded.list_status, …)` on a NOT NULL column always takes the excluded value and reintroduces the clobber bug. Split "enrichment patch" vs "user patch" APIs |
| Pins/absences/routes immune to enrichment upsert | own tables | `CLONE` |
| Absence TTL **7 days**; bind clears absence | `ABSENCE_TTL_SECONDS = 7*24*60*60`; ROD-347 tests | `CLONE` |
| Preferred re-route uses route stamp vs live pref | ROD-398 / `provider_routes` | `CLONE` intent on `show` id |
| Still-airing totals must not freeze aired-so-far as finale | ROD-419: clear a stale total only when `enrichment_fetched_at IS NOT NULL AND total_episodes IS NULL AND is_still_airing(status)`; never stamp partial field sets | `CLONE` on enrich write rules |
| Hard delete cascades progress/cache/bindings/pins/absences/routes | zigoku's `deleteAnime` is **binding-scoped** and never touches canonical/pins/absences/routes | `FIX-IN-RUST`: show-wide cascade from the show PK is new behavior, not a clone |
| Cover URL never downgrades absolute → relative | ROD-267: case-sensitive `GLOB 'http://*' / 'https://*'` CASE in the upsert; `http`-prefixed garbage neither sticks nor clobbers | `CLONE` |
| Blank romaji never wipes a stored title | zigoku gated canonical title behind a real-romaji flag (ROD-312, seed vs healed) | Adapted: `NULLIF(…, '')` in the merge treats blank as absence. The full seed-flag gate is not ported: every `Enrichment` here is AniList-sourced, provider seeds never reach these writers (ROD-434 review) |
| Migration completion is a real runtime check | zigoku checks in every build mode and unwinds via errdefer; a strippable assert is exactly the half-applied-schema bug | `CLONE`: no `debug_assert!` here |
| WAL flip has its own retry loop | `busy_timeout` does not cover the initial WAL PRAGMA; zigoku retries 100×5ms | `CLONE` mechanism |
| Enrichment heal TTL is status-aware | finished 30d / releasing 1d / else 7d | `CLONE` the shape or reject explicitly in M1 |
| Episode labels are free text | TEXT episode column | `CLONE` |
| History group order by ListStatus | domain `group_order` | `CLONE` (UI reads show rows) |

---

## 6. zigoku migration ladder (reference only)

Do **not** port v1…v18 as-is. Listed so archaeology in §2 has a map.

| Ver | Summary |
|---|---|
| 1 | `anime` + `episode_progress` + `episode_cache`, PK provider pair |
| 2 | year/status/description/score |
| 3–4 | `history_visible` + backfill hide search pollution |
| 5–10 | enrichment widening + airing/country |
| 11 | synced_status/progress |
| 12 | allanime→senshi re-key |
| 13 | `app_meta` |
| 14 | `canonical_anime` + `canonical_id` link + backfill |
| 15 | `provider_pins` |
| 16 | `provider_absences` |
| 17 | retire anipub; unbound orphan salvage |
| 18 | `provider_routes` |

sabigoku starts a **new** ladder implementing §3 directly.

---

## 7. Store API shape (intent)

Not a method-for-method port of `Store` in zigoku. Capabilities the TUI/sync need:

- open/migrate/close (WAL, FKs, busy timeout) on **sabigoku's own DB path only**
- upsert / get **catalog_cache** by `anilist_id` (Browse/Discover/detail paint)
- upsert show enrichment by `anilist_id`; promote from catalog_cache → show
- upsert user state (status, progress, rating, notes)
- list history (library `show` rows), get show, delete show (cascade)
- bind / unbind provider; list bindings for show; lookup show by `(provider, provider_id)`
- pin / clear pin; mark/check/clear absence; get/set route stamp
- episode progress get/set/recompute-from-rows; episode cache get/set/invalidate
- sync dirty set (snapshot vs live status/progress)
- meta get/set

Exact signatures wait for Rust modules; this list is the capability ceiling for M1
store work.

---

## 8. Locked decisions (this pass)

| ID | Decision |
|---|---|
| L1 | Episode equality = **identical label string**. Mismatch repair = future human-in-the-loop UX, not store logic (§3.4) |
| L2 | **`catalog_cache` is required and durable** for Browse/Discover/detail metadata (§3.5) |
| L3 | **No zigoku data plane.** Independent store; importer is a later standalone issue if ever (§3.6) |
| L4 | **Library membership is an explicit column** (`library_added_at`), stamped set-once inside P/reveal, user status writers, and successful `recordPlay` only; never by grid open, probe, prewarm, enrich, or sync pull (§3.7). `completed` ratchets progress only, not membership |
| L5 | **`genres` / `studios` are JSON string arrays** in TEXT columns (ratified with ROD-434). The Rust layer owns the encoding; SQL never splits or matches inside them. Replaces zigoku's `'\n'`-joined encoding |

## 8b. Still open

(O1/O2/O4 were promoted into locks L1–L3; numbering kept stable.)

| ID | Question | Lean |
|---|---|---|
| O3 | Multi-cour: one AniList id per cour already; confirm no "franchise PK" | One row per AniList media id (`CLONE` AniList model) |
| O5 | `UNIQUE (provider, provider_id)` if a provider id can map to two AniList ids (rare wrong bind) | Keep UNIQUE; re-bind is delete+insert; log conflicts |
| O6 | Whether `last_provider` on progress is worth storing | Optional audit column; not required for v1 |
| O7 | catalog_cache TTL / eviction policy (size cap vs time) | Serve stale OK; refresh policy belongs with runtime (04) + AniList rate limits |

---

## 9. Primary sources (freeze)

- `src/store.zig` — schema, migrate, upsert, pins/absences/routes, history load
- `src/domain.zig` — enums, afterPlay, isStillAiring, titles, episode helpers
- `src/provider_migrate.zig` — only if still referenced at freeze for one-shots
- Header comment on `store.zig` ("Why anime is keyed on (source, source_id)") — the
  justification we are consciously rejecting for the port
- CHANGELOG known issue: resume marker after source switch (feeds §3.4 / 07)

---

## 10. Adversarial checklist (for ROD-430)

- [ ] Can an implementer create a library entry without inventing a fake provider?
- [ ] Can preferred-provider switch avoid rewriting user-state primary keys?
- [ ] Is search pollution impossible by construction (`catalog_cache` ≠ `show`)?
- [ ] Does detail-card paint have a durable cache path (no mandatory network on every open)?
- [ ] Are pins/absences/routes clearly non-enrichment?
- [ ] Is episode TEXT + string-equality progress locked, with mismatch deferred to UX?
- [ ] No leftover requirement to implement `SOURCE_UNBOUND`, `canonical_id` dual spine, or zigoku DB open/import?

