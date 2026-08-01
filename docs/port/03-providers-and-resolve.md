# 03 · Providers and resolve

| Field | Value |
|---|---|
| Status | `review-stable` |
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
| **Resolve orchestration** | Given `anilist_id` (+ last-used/pref, ROD-525), pick provider, fetch grid, play, fallback, warm. |
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
| `by_name(name)` | Owner of a persisted binding key. **Must** use this for bound rows, never `primary()` (fetching a bound id on the wrong provider silently corrupts the binding). `None` = retired provider. |
| `preferred(name)` | Named or `primary()` if empty/unknown |
| `preferred_searchable(pref)` | First entry of `ordered(pref)` whose `supports_search` is true; `None` when none can. **The CLI query path only** (06 §7.3, ROD-491); every other path binds one provider, because a provider id is meaningless on another |
| `ordered(pref)` | Preferred first, then construction order for the rest (ROD-344) |

`supports_search` defaults to **true**: tier-C search is the norm, so a provider
that cannot search must override it. A provider that forgets becomes eligible
for the CLI binding and fails at runtime instead of being skipped. The roster
test over `default_registry` pins the live lineup against exactly this drift.

### 3.3 When preference applies

**Last-used (per show) or global `preferred_provider`** shapes **new** resolution
and the fallback walk snapshot (§5.1, ROD-525). Paths that already own a concrete
binding use `by_name(binding.provider)`.

Under sabigoku identity (02): almost every open is AniList-keyed. Last-used and
preference are first-class. zigoku's legacy "provider-keyed `.direct` must not
re-route" still applies when the UI is explicitly on a **chosen binding** (a
manual walk, `v`), not when inventing a provider for a naked AniList id.

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

### 4.1 Classifier: one entry path (ROD-525; Path 2 was never wired)

Round 1 verification (ROD-430) found zigoku has **no single classifier** and
described the port as `CLONE`ing a two-path split, canonical open vs History
open, plus a third manual-flip path. **ROD-525 verification against the sabigoku
code (2026-08-01) found that split was never real:** `resolve::open_history` and
the pin hard-restriction below it have **no caller**. Every History open has
always run the canonical-open algorithm. This is doc/code drift discovered at
ROD-525, not a deliberate simplification, and it is recorded here rather than
silently deleted: the second path in the original port design was dead on
arrival, and the false-green ledger row it produced is retired in a later code
chunk (not this one).

**The one open algorithm (Browse open, Discover zoom, add-to-watchlist, History
open, all of them).** Last-used folds into *effective preference*, nothing more:

```
open(show: AniListId):
  order = ordered(last_used(show) orelse preferred)  // last-used first, then construction
  for p in order: if binding(p) exists → Bound        // ANY binding wins tier 0:
                                                       // a binding the walk didn't
                                                       // start on still wins here
  for p in order: if key = p.canonical_key(show) → TierA { p, key }
  → NeedsSearch (tier C across all non-absent providers, same order)
```

No pin arm, no hard restriction, no separate route-preferred call: last-used is
one entry in `ordered`, the same mechanism `preferred` already used. A History
open and a Browse open of the same show run the identical function; there is no
second algorithm left to describe.

**Manual walk (`v`).** Not a resolve entry path: it is a live action on an
already-open grid, not a way of arriving at one. Full mechanics moved to §5.2
(absence-probe scope) and 05 §10.5 (the walk contract); this section no longer
lists it as a third path, because outside its own single hop it does not touch
the classifier above at all.

zigoku also had `.direct` for provider-keyed selections where `sel.id` was already
a provider handle. Under 02, that collapses to **Bound** or an explicit binding
context. Do not reintroduce "stringified anilist_id as fake provider id" as a
library key.

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

## 5. Last-used and absences

All keyed by **`anilist_id`** (02). Own tables; enrichment upserts never touch them.

### 5.1 Last-used (ROD-525; retires the pin)

The two-tier preference (global `preferred_provider` + per-show pin) is retired.
In its place: the app remembers the provider each show **last successfully
served from**.

- At most one provider per show, or unset.
- **Written only at a landing** (a confirmation write, never speculative), and
  only when it differs from the walk-order head. A landing is a successful
  episode fetch or stream resolve on that provider (§6.1, §6.3), auto or manual.
- Opens start at last-used (§4.1: `ordered(last_used orelse preferred)`) and walk
  on failure like any other open; every landing updates it, including a landing
  onto a provider a show had migrated away from before.
- No hard restriction: last-used is one entry in `ordered`, and tier 0 (**any**
  binding) still wins over it exactly as it always did (§4). A show whose
  last-used provider lost its binding just falls through to the next provider in
  `ordered`, the same as a stale `preferred_provider` always has.
- Per-show **language** intent, the pin's original motive, is deferred to a
  future epic: it will eventually filter `ordered` before last-used is applied.

### 5.2 Absence (negative cache)

- Row `(anilist_id, provider, checked_at)` = definitive not stocked.
- **TTL @ freeze: 7 days.** Fresh absence ⇒ skip automatic probe/search; **bindings always win.**
- **Manual-walk probe scope (ROD-525):** the `v` walk probes through fresh
  absence on the **first hop only**, the provider the user pressed toward.
  Every hop after that respects absence like any auto walk. Probing the whole
  circle would relocate the ROD-524 fetch fan-out from press-time to miss-time;
  one paid hop plus a silent circle keeps a `v` press bounded regardless of how
  many providers are stale.
- Successful bind **deletes** absence for that pair (bound and absent never coexist).
- Availability UI: `unchecked` | `bound` | `absent` (derived; only absence is stored).

### 5.3 Route stamp and forced-preferred: retired, no replacement guard (ROD-525)

The route-stamp mechanism (`ProviderRoute`, the settled-pref comparison, the K-2
forced-preferred continuation walk) is **deleted, not replaced**. Its entire job
was protecting a **speculative** write: the stamp had to land *before* the fetch
it gated ("stamp-before-fetch, so a miss cannot loop forever") because a stale
stamp could re-arm the same forced probe on every later open. Last-used (§5.1)
is written only at a confirmed landing, never speculatively, so the failure mode
the stamp guarded against cannot occur: a landing that never happens cannot loop,
and a landing that does happen is, by definition, not a miss. No replacement
guard is needed because there is nothing left for a guard to protect.

The K-2 bug (07 bug ledger) closes as **moot**, retired along with the mechanism
it lived in, not fixed within it: there is no forced single-provider probe left
to dead-end.

---

## 6. Open / play pipelines

### 6.1 Episode grid open (canonical show)

High-level:

1. Clear stale in-flight bind/walk/play-search want (walk hops reinstall their walk).
2. Refresh last-used + availability cache for the open `anilist_id`.
3. Maybe enrichment refresh (independent of episode cache).
4. Classifier (§4.1) with `order = ordered(last_used orelse preferred)` → bound / tier A / needs search.
5. Episode list cache hit → paint grid; else spawn worker.
6. User-driven open: **do not** arm resume-demote. Only auto-resume landing does.
7. On landing, write last-used when the landed provider differs from the walk-order head (§5.1).

**Do not join** a prior episode worker on the UI thread. Detach + drain; keep-check drops
stale results (`ZIG-SHAPE` → Rust: generation token / `anilist_id` + cancel flag).

**FK order (ROD-327):** persist the `show` identity row **before** caching
episodes or minting bindings, so foreign keys hold on the first resolve.

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
3. Spawn mpv with referer/UA/cloak/sub flags from `StreamLink` (§6.3.1).
4. Observe position; on meaningful progress, write `episode_progress` (02 keys).
5. On `MpvOpenFailed` (exit 2): retry budget with re-resolve; then play-fallback hop.
6. Mid-play prewarm siblings so a source flip is tier-0.

#### 6.3.1 mpv invocation and IPC (byte-critical enough to table)

| `StreamLink` field / need | mpv argv |
|---|---|
| `url` | positional |
| `referer` | `--http-header-fields-append=Referer: {r}` |
| `user_agent` | `--user-agent={ua}`: **dedicated flag, replaces the Lavf default** (Cloudflare 403s the default; header-append would send two UAs) |
| any `http` url | `--stream-lavf-o=multiple_requests=1,icy=0` (constant: keep-alive across HLS segments + drop Icy-MetaData, gentler on CF rate scoring) |
| `sub_url` | `--sub-file={s}` plus `--sub-pos=92 --sub-bold=yes` |
| `cloaked_segments` | `--demuxer-lavf-o=allowed_extensions=ALL` |
| title | `--force-media-title={title}`, `--title=sabigoku - ${media-title}` (property expansion, injection-safe) |
| IPC | `--input-ipc-server={runtimeDir}/sabigoku-mpv-{uid}-{pid}-{counter}.sock` (unique per launch) |
| resume | `--start={secs}` when start > 0 |
| aniskip | `--script={path} --script-opts={opts}` |

**Sanitize first (ROD-92):** provider-supplied referer/UA are untrusted; strip
CR/LF before they touch argv (header injection).

**IPC is push-based, not polled.** Watcher connects to the socket (connect budget
~2s: 40 × 50ms), sends `observe_property` for `time-pos` (id 1) and `duration`
(id 2), then blocking-reads newline-delimited JSON `property-change` events.
**Meaningful position** = finite and > 0; it gates both retry eligibility (only
retry `MpvOpenFailed` with no meaningful playback yet) and whether the final
position persists.

**Resume start rule:** every play calls progress-get for the episode. Start = 0
if `fully_watched`, or position/duration ≥ 0.80, or position ≤ 0 / non-finite;
else saved position minus `resume_offset_sec` (default 5), saturating at 0.
zigoku reads checkpoints across sibling bindings (canonical join, freshest wins);
the 02 keying gives that by construction. Do not re-scope resume reads to a
single binding or resume breaks across a fallback hop.

### 6.4 Fallback walk (ROD-346)

After a **failed** episode fetch or stream open, if the show has `anilist_id`:

- Snapshot `ordered(effective_pref)` at walk start (mid-walk pref changes must not reshuffle).
- Mark the failed provider tried; advance to next.
- Per hop: bound id → fetch; else skip fresh absence (unless `manual`); else tier A key;
  else single-provider tier C search.
- One hop per failure event (single-flight with episode/play guards).
- Play continuation: after hop grid lands, **remap episode** (exact raw label, else
  1-based ordinal) and relaunch; walk stays armed (no ping-pong of fresh walks).
- **Landing writes last-used** (§5.1) exactly as any other landing does, whether
  the walk was armed by a play/fetch failure or by a manual `v` (05 §10.5),
  including a landing on a provider a show had already migrated away from.
- Rescue cancels background prewarm (CDN budget).

**Resume demote (ROD-229):** auto-resume open sets `resume_landing_pending`. Failure
demotes to History list only when the **whole walk** is exhausted, not on intermediate
hops. User-driven opens never arm demote.

### 6.5 Prewarm

After successful add/play: background-probe unbound providers (no binding, no fresh
absence) so later flips are tier 0. Yields to user-facing resolve and active
fallback. Construction order for candidates (pref is not required).

Dedup at freeze is **not** a per-session set: a 32-slot ring of attempted
`anilist_id`s (round-robin eviction, so a show can re-attempt after 32 others)
plus a **30s app-wide spacing floor** between walk starts. `blocked()` also gates
on: walk already active, add or play resolving, fallback active. Port lean: strict
per-session set + the 30s spacing (deliberate simplification; say so in 08 if it
sticks).

### 6.6 Episode label remap across providers

`map_episode_index(list, raw, ordinal)`:

1. Exact raw string match.
2. Else 1-based ordinal into the sorted list if in range.

Aligns with 02 **L1**: string equality is identity; ordinal is best-effort hop UX, not a
second progress key.

### 6.7 Fetch guard (SSRF) — all provider-supplied URLs

Every URL a provider hands back (stream, embed, master playlist, subtitle probe,
cover) passes one guard before fetch, **always paired with redirects disabled**
(a followed 3xx would defeat the check). `CLONE` of zigoku `util/fetchguard.zig`
(0.4.6 hardening, ledger R-10):

- Scheme allowlist: `http` / `https` only.
- Reject URL userinfo (`user@host`).
- Validate the **decoded** host (defeats `127%2e0%2e0%2e1` percent-encoding).
- Block `localhost` and `*.localhost` by name.
- IP-literal denylist: IPv4 `0/8`, `10/8`, `127/8`, `100.64/10` (CGNAT),
  `169.254/16` (link-local / cloud metadata), `172.16/12`, `192.168/16`, `≥224`
  (multicast/reserved/broadcast); IPv6 `::`/`::1`, `fe80::/10`, `fc00::/7`,
  IPv4-mapped `::ffff:a.b.c.d` recursed through the v4 rules.
- Reject alternate IPv4 spellings in a non-literal host: all-decimal, `0x` hex,
  short forms like `127.1`; reject `:` in the host.
- Known residual (accepted, documented): DNS rebinding — a public name resolving
  to a private IP at connect time is not caught without a resolve-then-validate
  hook.

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
The single authoritative threshold table lives in 02 §4b. The mpv-open-failed
retry toast copy at freeze: `stream didn't open — try again`.

---

## 8. Live providers at freeze (enough to reimplement)

Detail protocols stay in provider modules + golden tests. This section is the map.

### 8.1 megaplay (primary)

- **Tier A:** `provider_id = stringified mal_id`. No MAL → no key; **no tier C**
  (`search` unsupported).
- Episodes: listing-less; uses `count_hint` / expected episode count to mint labels.
- Stream: MAL-based embed route; may attach softsub `sub_url`.
- Softsub pick (0.4.3 fix, ledger R-7): metadata baseline (host `default` flag →
  first English-labeled captions → first captions); then, sub translation only and
  ≥2 English caption tracks (cap 6 probes), **cue-count refinement**: fetch each
  candidate `.vtt` (guarded §6.7, redirects refused), count cues, upgrade only on
  strictly more (never downgrade; a failed candidate fetch just drops it). This is
  what keeps a signs-only "default" track from beating the dialogue track.
- Absence: empty/miss paths mark not stocked when appropriate; unsupported search must
  **not** poison absence for "no mal yet" cases.

### 8.2 senshi

- **Tier A:** stringified `mal_id` (same shape as megaplay).
- Search: POST filter API (tier C recovery when no MAL).
- Episodes / embeds by mal_id; covers via poster CDN.
- HLS may use **cloaked segments** (`cloaked_segments = true` on `StreamLink`).
- Softsub pick is metadata-only (host default → English-labeled → first); no cue
  probing here.

### 8.3 allanime (backstop)

- GraphQL + persisted query hashes; site facts quarantined in module.
- Stream blob: AES-256-GCM `tobeparsed`. Key = `sha256(GCM_SEED)` where the seed
  is a **hardcoded constant** (not derived from any response). Layout after
  base64-decode: 1-byte prefix (discard) + 12-byte nonce + ciphertext + 16-byte
  tag, empty AAD. **Normative artifact:** sabigoku `examples/spike_stream.rs`
  carries the seed and a byte-identical golden vector from zigoku's test fixture;
  golden test is the contract.
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
- Modes from config `skip_mode`: `none` | `intro` | `outro` | `both`; unknown
  string → both (a typo must not disable skip). See 06 §2.2.
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

- get/set last-used, absence (fresh?)
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
- [ ] Last-used leads `ordered`; any binding still wins tier 0 regardless (§4.1, §5.1)
- [ ] Last-used writes only at a landing, never speculatively (§5.1)
- [ ] Manual walk probes fresh absence on the first hop only, not the whole circle (§5.2)
- [ ] Resume demote only on auto-resume walk exhaust
- [ ] No `unbound` / provider-primary library identity required
- [ ] Tier-C thresholds and id-match vetoes named
- [ ] Play error classes map to user copy + hop/retry policy
- [ ] Episode remap uses string equality then ordinal (02 L1)
