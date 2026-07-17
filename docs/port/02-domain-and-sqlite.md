# 02 · Domain and SQLite

| Field | Value |
|---|---|
| Status | `draft` |
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
4. **No play provider ⇒ no binding row**, not a fake `unbound` source. History
   lists `show` rows the user engaged with; playability is "has at least one
   binding" or "resolve can find one."
5. **Search/Discover catalog is not the library table.** AniList hits may be
   cached separately or kept ephemeral; they do not insert provider-PK rows that
   need `history_visible` laundering.
6. **Pins, absences, routes stay off the enrichment upsert path** (keep zigoku's
   table-split invariant; it was right).

### 3.2 Logical entities

| Entity | Key | Owns |
|---|---|---|
| **Show** | `anilist_id` | Titles, cover, scores, airing, genres, …; list status; progress count; ratings/notes; sync snapshots; enrichment freshness |
| **ProviderBinding** | `(anilist_id, provider)` unique; `provider_id` opaque | How to talk to a stream source for this show |
| **EpisodeProgress** | see §3.4 | Resume + fully_watched per episode label per translation |
| **EpisodeCache** | `(anilist_id, provider, translation)` or `(provider, provider_id, translation)` | Provider episode label lists + TTL |
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
    genres                      TEXT,          -- storage encoding TBD; display list
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
    added_at                    INTEGER NOT NULL,
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

CREATE TABLE app_meta (
    key   TEXT NOT NULL PRIMARY KEY,
    value TEXT NOT NULL
);
```

### 3.4 Episode progress keying (decision + risk)

**Decision:** progress rows key by **`(anilist_id, translation, episode_label)`**,
not by provider. Watch state belongs to the show.

**Risk (known from zigoku):** providers disagree on labels and numbering. zigoku
CHANGELOG already names resume-one-behind after source switch. Provider-scoped
progress would "fix" that by forking state; show-scoped progress needs an
explicit policy when labels disagree:

| Policy | Notes |
|---|---|
| **A. Label identity** (default above) | Same string = same ep. Simple; mismatches are user-visible scars. |
| **B. Prefer pin's label space** | Progress only meaningful under active binding; remap on pin change. Complex. |
| **C. Integer-only core + label map** | Only when both sides are pure ints; specials stay provider-local. |

**Port default: A**, document mismatches in [`07-bug-ledger.md`](07-bug-ledger.md)
as `FIX-IN-RUST` candidates (better remap UX later), not as a reason to put user
state back on `(provider, provider_id)`.

`episode` stays **TEXT**, never INTEGER. Labels are not always integers (`1.5`,
`SP1`). That zigoku choice is `CLONE`.

### 3.5 What replaces `history_visible` and `unbound`

| zigoku | sabigoku |
|---|---|
| Search hit upserted into `anime` with `history_visible = 0` | Do not insert into `show` until the user engages (add/play/plan), **or** use a separate catalog cache table with no user-state columns |
| `source = 'unbound'` sentinel binding | `show` row without `provider_binding` rows; UI still lists the show |
| COALESCE join binding ↔ canonical on every History load | History is `SELECT … FROM show` (plus optional binding aggregate for "which sources") |

If we need offline AniList catalog cache for Discover, give it its **own** table
(`catalog_cache` / similar). Never overload the library show row.

### 3.6 Fresh DB vs import

**Default for M1:** sabigoku owns a **new schema** (version ladder starts at 1 for
this shape). No obligation to open a zigoku `user_version = 18` file.

**Optional later:** one-shot importer (zigoku DB → sabigoku) that:

1. builds `show` from `canonical_anime` ∪ distinct `anilist_id` on visible rows
2. folds user state across sibling bindings (max progress, status precedence TBD)
3. emits `provider_binding` per non-`unbound` row
4. rewrites progress onto `anilist_id` keys (drop or collide-resolve provider forks)

Importer is out of scope for this chapter's first implementable store; track as
`OPEN` / follow-up ticket when parity users need it.

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
| `expected_episode_count` | airing uses next_airing-1 floor; settled uses total (ROD-359) |
| Resume ratios | `WATCHED_RATIO = 0.95`, `NATURAL_END_RATIO = 0.80` (`CLONE`) |

**Show vs binding in the type model**

- Catalog/search DTOs from AniList carry `anilist_id` as required (not optional
  enrichment).
- Provider search (tier-C binding) returns provider ids that are **candidates to
  link**, not library identity.
- In-memory TUI "focused show" should be `anilist_id`-keyed; active binding is a
  field on that focus, not the focus key itself.

---

## 5. Behaviors to preserve (storage-level)

These are product invariants; only the tables they sit on change.

| Invariant | zigoku evidence | sabigoku note |
|---|---|---|
| Concurrent open migrates atomically | `BEGIN IMMEDIATE` whole ladder; busy timeout; re-read version under lock (ROD-287) | `CLONE` mechanism |
| Schema too new ⇒ hard error | `SchemaTooNew` | `CLONE` |
| Single `user_version` ladder, one bump at end | migrate() @ freeze | `CLONE` |
| Enrichment fieldset version heals columns without full TTL | `ENRICHMENT_FIELDSET_VERSION = 5` | `CLONE` idea; renumber for our column set |
| Upsert must not clobber user state with search/enrichment | upsert COALESCE / preserve list_status, progress, … | Show upsert splits "enrichment patch" vs "user patch" |
| Pins/absences/routes immune to enrichment upsert | own tables | `CLONE` |
| Absence TTL; bind clears absence | ROD-347 tests | `CLONE` |
| Preferred re-route uses route stamp vs live pref | ROD-398 / `provider_routes` | `CLONE` intent on `show` id |
| Still-airing totals must not freeze aired-so-far as finale | ROD-419 | `CLONE` on enrich write rules |
| Hard delete cascades progress/cache/bindings | FK ON DELETE CASCADE | `CLONE` from show PK |
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

- open/migrate/close (WAL, FKs, busy timeout)
- upsert show enrichment by `anilist_id`
- upsert user state (status, progress, rating, notes, visibility-of-engagement)
- list history (engaged shows), get show, delete show (cascade)
- bind / unbind provider; list bindings for show; lookup show by `(provider, provider_id)`
- pin / clear pin; mark/check/clear absence; get/set route stamp
- episode progress get/set/recompute-from-rows; episode cache get/set/invalidate
- sync dirty set (snapshot vs live status/progress)
- meta get/set

Exact signatures wait for Rust modules; this list is the capability ceiling for M1
store work.

---

## 8. Open questions

| ID | Question | Lean |
|---|---|---|
| O1 | Episode progress policy when provider labels disagree (§3.4) | Policy A until UX forces B/C |
| O2 | Separate `catalog_cache` table vs pure ephemeral AniList responses | Ephemeral first; add cache if Discover needs offline |
| O3 | Multi-cour: one AniList id per cour already; confirm no "franchise PK" | One row per AniList media id (`CLONE` AniList model) |
| O4 | zigoku DB importer | Defer; new schema first |
| O5 | `UNIQUE (provider, provider_id)` if a provider id can map to two AniList ids (rare wrong bind) | Keep UNIQUE; re-bind is delete+insert; log conflicts |
| O6 | Whether `last_provider` on progress is worth storing | Optional audit column; not required for v1 |

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
- [ ] Is search pollution impossible by construction (no shared table)?
- [ ] Are pins/absences/routes clearly non-enrichment?
- [ ] Is episode TEXT + show-scoped progress explicit about mismatch risk?
- [ ] No leftover requirement to implement `SOURCE_UNBOUND` or `canonical_id` dual spine?
