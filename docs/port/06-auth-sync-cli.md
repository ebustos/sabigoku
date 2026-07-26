# 06 · Auth, sync, CLI

| Field | Value |
|---|---|
| Status | `review-stable` |
| Ticket | ROD-427 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Spine | [02](02-domain-and-sqlite.md) show-keyed library · [01](01-modules.md) entrypoints · [04](04-tui-runtime.md) workers · [05](05-behavior-contracts.md) TUI arms |
| Nature | **Port design.** Brand strings `zigoku` → `sabigoku` in paths/copy; contracts stay. |

---

## 1. Paths layout

Platform base dirs (Linux XDG / macOS Library). **Windows: unsupported** at freeze
(no silent scatter). App segment name: sabigoku uses **`sabigoku`** under each base
(zigoku used `zigoku`).

| Dir | Linux default | Holds |
|---|---|---|
| **config** | `~/.config/sabigoku` | `config.*`, `auth.*` |
| **data** | `~/.local/share/sabigoku` | `sabigoku.db` (name open), debug log |
| **cache** | `~/.cache/sabigoku` | covers, AniSkip scripts, update_check cache |
| **runtime** | `$XDG_RUNTIME_DIR/sabigoku` or `/tmp/sabigoku` | mpv IPC sockets (must work without HOME) |

Rules:

- `ensureDir` is best-effort mkdir-p; real failure surfaces on open.
- Settings display may collapse `$HOME` prefix to `~` (boundary-safe: `/home/rod` must not swallow `/home/rodney`).
- **Independent store (02 L3):** only sabigoku data dir; never open zigoku paths by default.

---

## 2. Config

### 2.1 File

| | |
|---|---|
| Path | `{configDir}/config.zon` @ freeze → **sabigoku may use TOML/JSON** (08); same keys |
| Load | **Total:** missing/corrupt → defaults; never wedges startup |
| Save | Surfaces errors (Settings / quit report) |
| Unknown fields | **Ignored** (forward compatible) |
| Max size | 64 KiB @ freeze; oversized → defaults |

### 2.2 Keys (defaults)

| Key | Default | Meaning |
|---|---|---|
| `mpv_path` | `"mpv"` | PATH or absolute |
| `default_quality` | `"best"` | domain Quality |
| `translation` | `"sub"` | `sub` \| `dub` (else sub) |
| `resume_offset_sec` | `5` | rewind context on resume |
| `skip_mode` | `"both"` | none\|intro\|outro\|both; unknown → both (typo must not disable) |
| `image_protocol` | `"auto"` | cover protocol preference |
| `cover_art` | `true` | |
| `kanji_chips` | `true` | |
| `palette` | `"terminal_ghost"` | DESIGN themes |
| `landing` | `"history"` | history\|browse\|last_watched |
| `title_language` | `"romaji"` | romaji\|english\|native |
| `discover_cover_concurrency` | `4` | clamp **[1, 16]** at read |
| `preferred_provider` | `""` | empty = registry construction order |
| `anilist_sync_enabled` | `true` | master switch; off = inert sync rail, **token stays** |
| `check_for_updates` | `true` | boot GitHub check gate |

Enums: unknown strings degrade at call site to safe defaults (sub, history, romaji, best).

---

## 3. Auth token

### 3.1 File

| | |
|---|---|
| Path | `{configDir}/auth.zon` (or format twin of config) |
| Separation | **Never** inside config: secret must not ride Settings round-trips |
| Mode | Create **`0600`** atomically (bearer never group/world-readable mid-write) |
| Load | Total → empty signed-out on missing/bad |
| Max size | 16 KiB @ freeze |

### 3.2 Record shape

```
Auth {
  anilist: {
    access_token,      // empty = signed out
    token_type,        // default "Bearer"
    expires_at,        // unix; 0 = undated (prefer 401 over refuse)
    user_id,           // AniList user id; MediaListCollection needs > 0
    user_name,
  }
}
```

Nest under `.anilist` so future MAL/Kitsu blocks fit.

### 3.3 Hardening (`CLONE`)

| Rule | Why |
|---|---|
| Control bytes (`ch < 0x20`) in token → treat as signed-out | CR/LF trips strict HTTP clients' line asserts (release abort); a bare LF can inject headers |
| `is_expired(now)` only if `expires_at != 0 && now >= expires_at` | Zero expiry stays live until 401 |
| ~1 year JWT, **no refresh** | Implicit Grant reality |
| Never persist before live Viewer verify | `completeLogin` order |

---

## 4. OAuth login

### 4.1 Protocol

- **Grant:** AniList OAuth **Implicit** (`response_type=token`).
- **Client id @ freeze:** `43536` (sabigoku needs its own AniList app registration; treat as config/build constant, not hard product).
- **Loopback port:** `8765` single source of truth for redirect + bind.
- **Redirect URI:** `http://localhost:8765` (registered with AniList; not always sent on authorize URL).
- **Authorize URL:**  
  `https://anilist.co/api/v2/oauth/authorize?client_id={ID}&response_type=token`  
  plus `&state={csrf}` for loopback.

### 4.2 Shared core: `completeLogin`

I/O-free relative to UI; steps:

1. Extract `access_token` (and optional `expires_in`) from raw redirect / query / paste.
2. Token length floor (~20); else `no_token`.
3. **Viewer query** against GraphQL with bearer (deadline ~10s for TUI join safety).
4. No Viewer → `rejected`.
5. Build Auth record (`expires_at = now + expires_in` if provided).
6. `auth.save` → `ok` or `save_failed`.

**Never** write file before verify succeeds.

### 4.3 CLI modes

| Mode | Behavior |
|---|---|
| `login` (default) | Loopback listener; open browser; wait for callback |
| `login --paste` | Print authorize URL; user pastes redirect line (SSH-safe) |
| Loopback bind/nonce fail | `LoopbackUnavailable` → fall back to paste |

Already signed in: print name; re-run **replaces** token.

### 4.4 Loopback server (byte-critical bits)

Implicit Grant puts token in **`location.hash`** (browser-only). Server never sees
the hash on first GET.

1. First hit (any non-callback) → **relay HTML** with script:  
   `location.replace("/callback?" + location.hash.substring(1));`  
   **Do not reformat/rewrap this script** (zigoku comment: byte-critical).
2. `/callback?...` → CSRF: `state` query must match minted nonce; else `bad_state`
   (never verify/persist).
3. Valid callback → `completeLogin` → success or fail HTML.
4. Success/fail pages scrub address bar ASAP:  
   `history.replaceState(null,'','/')` early in head.

**Cancel (TUI):** set cancel flag, then **dial localhost once** to wake blocked
`accept`. Worker skips posting `ConnectResult` on `.canceled` (no race on freed arena).

**No overall timeout** on CLI wait (browser may be slow). Per-connection read
deadline (**5s** @ freeze) so one stalled socket cannot wedge accept forever.
Bind is **IPv4 `127.0.0.1` only**, deliberately: pure-`::1` localhost hosts
cannot reach it (rare; Happy Eyeballs falls back to IPv4).

### 4.5 Post-login bootstrap

If login **persisted** a token (CLI), optional **bootstrap sync** may run (ROD-292).
Failed login → no sync. TUI connect: reload auth, arm bootstrap per 04/05.

---

## 5. AniList sync

### 5.1 Snapshot model

Each library show carries last server-accepted pair:

- `synced_status`, `synced_progress` (02 on `show`)

**Dirty** when engaged + has `anilist_id` and live pair ≠ snapshot (or snapshot null).

### 5.2 Order (law)

**Pull then push.** Never push-first on first contact: dirty never-synced rows would
blind-upsert and wipe AniList history (ROD-285 regression).

Round-1 verification: the protection is **structural**, not caller discipline.
Every TUI sync entry point funnels through one worker function whose first
statement is the pull; `pull_only` merely gates whether the push runs after. The
one push-only path anywhere is the quit flush, and it is explicitly skipped while
a pull is inflight. Entry points at freeze:

| Entry point | Gate | Runs |
|---|---|---|
| CLI `sync` | token present + unexpired | pull → reconcile → push (push skipped if pull hit 401/429/store error) |
| TUI launch refresh | connected + sync enabled | pull only |
| TUI action flush (ROD-291) | connected + sync enabled | pull → push (same shared worker as CLI order) |
| TUI post-connect bootstrap (ROD-292) | connected + sync enabled | pull → push |
| TUI quit flush | connected + sync enabled, **skipped if a pull is inflight** | push only, bounded best-effort (04 §11) |

There is no push-oriented fast path for action flush; do not build one.

### 5.3 Push (`pushAll`)

| | |
|---|---|
| Work list | Engaged, id-bearing, dirty rows only |
| Hidden/search-only | Not in push set (02: only library `show`) |
| Spacing | ~2s between row calls (AniList rate) |
| 429 | Sleep ~60s once, retry row; second 429 → stop run, rest stay dirty |
| 401 | Stop run immediately |
| Success | `markSynced` advances snapshot; success requires a **non-null `SaveMediaListEntry.id` in the body**, never HTTP 200 alone (a 200 without id advancing the snapshot silently loses the row) |
| Engaged-but-unlinked | summary lists their titles as an actionable list, capped at 12 + "and N more" (not just a count) |
| No token / expired | No-op summary flags |
| Per-row other errors | Count failed, continue |

### 5.4 Pull + reconcile (`pullAll`)

Requires token, not expired, `user_id > 0`.

Fetch `MediaListCollection`. For each local reconcile candidate joined by
`anilist_id`:

Fetch `MediaListCollection`: the **full remote list in one POST, unpaginated**;
it shares the 2MB response cap, so a huge AniList list can fail the whole pull.
Surface that failure, do not swallow it (`OPEN`: raise the cap or paginate).

**Candidates:** reconcile joins **library rows with an `anilist_id` only**
(zigoku: `history_visible != 0`, store comment "engaged + anilist_id with
snapshot. Not dirty-filtered"; sabigoku: `library_added_at IS NOT NULL`). Clean
rows are included: clean rows can still receive remote changes. Identity-only
rows are invisible to **reconcile**, so a merge can neither stamp membership nor
park user state on a show the user never engaged (02 §3.7). The O3 auto-import
(below) is the sole pull path that stamps membership, and only for an unmatched
WATCHING/REPEATING entry: it promotes an existing identity row rather than
merging it.

**Duplicate collapse, before the merge.** `MediaListCollection` groups entries,
and a media in custom lists appears once per group it belongs to, so the flat
list can hold several rows for one id. Those rows are views of a single
`MediaList` record, so in practice their progress and `updatedAt` agree; a
capture of an account with custom lists would be needed to show otherwise, and
none exists here. The collapse rule below is therefore **defensive**, not a fix
for an observed divergence: it decides what happens if copies ever disagree,
whether through an API change, a partial response, or a tampered one. They collapse to one pair per id by **`(updatedAt, progress)`**:
recency decides, and progress only breaks a tie. When a whole group is
unstamped the tiebreak is all that is left, so the fold degenerates to plain
max-progress there: order-independent, but carrying the same upward bias this
chapter rejects above. That is the floor of the guarantee, not the intent. Recency, not magnitude:
collapsing by progress would re-raise exactly what the merge below exists to
lower, since a correction is by definition the smaller number, and a stale copy
in any custom list would silently pin the old value forever.

Ties are ordinary. One edit fans across custom lists at a single stamp, and
AniList nulls `updatedAt` on entries untouched since the field landed, which maps
to `0` and can leave a whole group unstamped. The progress tiebreak keeps the
fold order-independent **for progress** in both cases rather than adopting
whichever group the server happened to serialize first. Two copies alike in both
stamp and progress but differing in status still resolve by wire order; no
signal exists to separate them, and no evidence says AniList emits that shape.

Deviation from zigoku, which dedupes at ingest by **unconditional last-wins wire
order** (`anilist.zig:605-611`) and never requested `updatedAt`
(`anilist.zig:560`). The max-progress collapse this replaced was sabigoku's own
invention, not a ported rule, and cited a 06 clause that did not exist
(ROD-497; see the backport ledger).

**Pure merge (`reconcile`), the total matrix.** `eff_base` = snapshot status,
or `planning` when the snapshot is null (first contact). `local_moved` =
local ≠ eff_base; `remote_moved` = remote ≠ eff_base:

| local_moved | remote_moved | status outcome | conflict |
|---|---|---|---|
| no | no | unchanged | no |
| no | yes | adopt remote | no |
| yes | no | keep local | no |
| yes | yes, same target | keep local (converged) | no |
| yes | yes, different | **keep local** | **yes** (stays dirty for push) |

- **progress** runs its own matrix against the snapshot's **progress** half
  (`0` on first contact). `local_progress_moved` = local progress ≠ base
  progress; likewise for remote. These are independent of the status flags in
  the table above and are evaluated separately:

| local_progress_moved | remote_progress_moved | progress outcome |
|---|---|---|
| no | no | unchanged |
| no | yes | **adopt remote, downward included** |
| yes | no | keep local |
| yes | yes | `max(local, remote)` |

  Deviation from zigoku, which maxes in every cell unconditionally (ROD-497; see
  the backport ledger). A raise-only rule cannot represent a corrected entry: the
  merge keeps the stale high local value, the snapshot re-baselines to the lower
  remote, and the resulting mismatch queues the row to push the stale value back
  over the correction. `max` survives only where it earns its keep, the
  both-moved race, so no watched episode is lost.
- **Cross-half synthesis guard.** Because the two matrices run independently they
  can land on `completed` with a progress adopted from behind it, a pair neither
  side held. When the status outcome is `completed` **and local's own status was
  already `completed`**, progress takes local's value as a **floor**
  (`max(merged, local)`), not as a hold: a remote advance above it still wins.

  The guard is deliberately on the synthesis, not on the merged status alone,
  and it holds a value **local already had** rather than reaching for the
  episode total. Snapping to the total looks like the `setListStatus` invariant
  (02 §4b) but is not safe here: `total_episodes` is cached enrichment that
  drifts, `completed` below the total is legal on AniList, and a status-only
  guard fires on settled rows that nobody edited. That mints watch data, the
  snapshot re-baselines to the raw remote, the row goes dirty, and the invented
  value is pushed and re-minted on every subsequent pull. `setListStatus` is a
  deliberate user gesture; reconcile runs unattended on a timer, and the two do
  not get the same licence.

  A remote that moved **both** halves is not a synthesis: the completion and the
  progress under it arrive as one edit and are adopted together.
- `REPEATING` is folded to `watching` at ingest, before the merge; the progress
  matrix runs the same either way.

After merge write:

- Snapshot re-baselines to the **raw remote pair** whenever the remote pair
  differs from the snapshot (status **or** progress), including first contact
  (`base == null`) and the conflict cell: server truth in the snapshot is
  exactly what keeps a kept-local row dirty for the next push. (Not the merged
  pair. Progress-only remote bumps rebaseline too; the status matrix above is
  only about status outcome.)
- **CAS / optimistic guard:** the UPDATE is guarded on the pre-merge local pair
  (`WHERE … AND list_status = ? AND progress = ?`); zero rows changed means a
  concurrent edit landed mid-reconcile → count `contended`, leave the row, retry
  next run. A real guard, not advisory. The merged pair and the snapshot land in
  **one** CAS-guarded UPDATE; when neither the local pair nor the snapshot
  changed, the row is skipped entirely.
- Unmatched remote ids: counted and listed. A WATCHING/REPEATING entry with a
  usable title seed is **auto-imported** as an add-only library row, adopting the
  remote pair as truth with a matching snapshot so it is born clean (never pushed
  back). Other statuses, and seedless/titleless entries, stay count-only. This
  deviates from zigoku's count-only freeze (ROD-467; see the backport ledger).
  The seed rides the same pull: the `MediaListCollection` query carries
  `media{title episodes}`, the rest backfills via the TTL enrichment repull.

### 5.5 Master switch

`anilist_sync_enabled = false` → TUI sync rail inert; **do not delete token**.

Documented fact at freeze (not an open lean): the **CLI ignores the switch
entirely**: both the `sync` subcommand and the post-login bootstrap sync gate only
on token presence + expiry, while every TUI entry point gates on connected +
enabled. Port decision: unify on respecting the switch everywhere (lean), or
document the asymmetry on purpose.

### 5.6 Port identity note

zigoku dirty rows keyed `(source, source_id)`. sabigoku: **one row per `anilist_id`
on `show`** (02). Sync join key is already AniList id; simpler and correct.

---

## 6. Update check / update command

### 6.1 Boot check (`updatecheck`)

| | |
|---|---|
| Gate | `check_for_updates` |
| Compare | Built-in version vs GitHub `releases/latest` tag |
| Cache | `{cacheDir}/update_check` two-line body: `checked_at\ntag` |
| TTL | **6 hours**; future-dated cache treated stale |
| Failure | Silent null (offline, 403, bad body) |
| Fetch deadline | ~3s |
| User-Agent | Required (GitHub rejects empty) |
| Result | Post `UpdateAvailable` only when strictly newer (semver) |

Repo URL @ freeze is zigoku's; sabigoku points at **its** GitHub repo.

### 6.2 `update` subcommand

Detect install method (pacman/AUR, brew, standalone); print correct upgrade path or
run install script for standalone. Version pin safety on raw.githubusercontent refs
(reject path metacharacters). Details can stay thin in M1; behavior: never brick,
clear copy.

---

## 7. CLI surface

### 7.1 Subcommands (first non-flag positional)

| Command | Role |
|---|---|
| (none) | TUI |
| `login` | OAuth (`--paste` optional) |
| `sync` | pull then push |
| `update` | self-update / instructions |

Subcommand detection: flags may precede (`--debug login` OK). After a real query word,
`login`/`sync`/`update` are **search text**, not subcommands.

### 7.2 Flags / modes @ freeze

| Flag | Role |
|---|---|
| `--debug` | Enable debug log (also env) |
| `--dub` / `--sub` | CLI play path translation |
| `--quality v` / `--quality=v` | CLI play quality (both forms) |

### 7.3 Default binary behavior

| Invocation | Behavior |
|---|---|
| `sabigoku` | TUI |
| `sabigoku <query>…` | zigoku's non-TUI path is a full secondary UI: **single provider** via `preferred()` (no fallback walk), interactive stdin picks, cache-first episodes, resume + AniSkip + recordPlay wiring, and the CLI's only nonzero exit (1) on failure. **Lean: defer** to post-M1 (01 O3); M1 behavior for a positional arg: print "not supported yet", exit 2 — never silently open the TUI |
| Bare flags only | TUI |

**Query provider binding (ROD-491, deliberate divergence from zigoku).** The row
above records the freeze: zigoku's CLI takes `preferred()` and hard-fails when
that provider cannot search (`main.zig:178-180`, "CLI does not walk the
registry"). Combined with megaplay as primary (03 §3.1) and megaplay having no
tier C (03 §8.1) and `preferred_provider` defaulting to empty, a stock
`zigoku <query>` is dead on arrival, exit 1. Faithful parity with an upstream
defect, verified against the freeze rather than assumed.

sabigoku binds `preferred_searchable(pref)` instead (03 §3.2): the first
provider in `ordered` that can search. An explicit `preferred_provider` is still
honoured whenever it can search, and when it cannot the run notes the
substitution on one line and proceeds rather than dying. The walk is **search
only**; every other CLI path still binds exactly one provider, because a
provider id is meaningless on another. `None` (no provider can search) prints
and exits 1, an exit-1 reason additional to the play path in §7.4. A future
engineer diffing against zigoku must not "fix" this back to a hard fail.

### 7.4 Exit / messaging

Sync/login print human summaries (signed-out, expired, rate limit, conflicts,
unmatched). Baseline fact at freeze: `login`/`sync`/`update`/usage always exit 0,
even on outcome failure; only the CLI play path exits nonzero (1). Making sync
scriptable with real exit codes is a deliberate deviation (`OPEN`).

**Play exit fold (ROD-473, deliberate divergence from zigoku).** sabigoku's
`player::play` (shared with the TUI) folds a *meaningful watch that then ends in
an mpv failure* into `Ok(position: Some)`; only a play with no meaningful
position ever observed returns an error. So the CLI persists that watch and
exits 0, where zigoku persisted and then re-raised (exit 1). Exit code reflects
whether the user got a meaningful watch, not mpv's raw process exit; the cause
lives in one place (`player::play`) for both surfaces. A future engineer diffing
against zigoku must not "fix" this back to a reraise.

**Accepted (ROD-473 red-team).** `config.mpv_path` is an arbitrary path handed to
`Command::new`; a hostile binary there can fabricate a position and land a false
"watched" row at exit 0. This is inherent to a user-writable `mpv_path` (config
write already implies local control) and is accepted, not guarded.

---

## 8. Env vars

| Var | Role |
|---|---|
| XDG_CONFIG_HOME / XDG_DATA_HOME / XDG_CACHE_HOME / XDG_RUNTIME_DIR | Path bases |
| HOME | Fallbacks + collapse |
| COLORTERM | truecolor hint for TUI (04) |
| `SABIGOKU_DEBUG` (zigoku: `ZIGOKU_DEBUG`) | Force debug log without the flag; truthy/falsy parse |

No token in env at freeze (file only). Keep it that way unless a test harness needs
injection.

---

## 8b. AniList client (shared surface)

Round-1 review: this surface had no owning chapter; it lives here because auth and
sync already own the AniList edge. Facts @ freeze:

| | |
|---|---|
| Endpoint | `https://graphql.anilist.co`, POST, deadline ~10s, response cap 2 MB |
| Queries | by-id enrich; search (`sort: SEARCH_MATCH`); discover per axis (`season`/`seasonYear` vars **omitted**, not sent null, for non-this_season axes); `Viewer` (login verify); `MediaListCollection` (pull, unpaginated §5.4); `SaveMediaListEntry` (push) |
| Fieldset | **One** shared field superset for by-id / search / discover: id, idMal, titles, episodes, duration, averageScore, status, season(+year), startDate, format, source, country, genres, main studios, rankings, nextAiringEpisode, description, coverImage.large. There is no card-vs-detail tier at the GraphQL layer; card vs detail is UI-side selection. (zigoku's narrow batch fieldset served only its migration tool: drop it) |
| Page sizes | Browse search **26**; discover feed **20** (AniList perPage cap 50) |
| Enrich contract | three-state (05 §8): metadata / confirmed-null / no-answer |
| Rate limits | sync push: 2s spacing, 429 → 60s sleep once → stop; **non-sync calls have NO 429 handling at freeze**: any non-200 collapses to no-answer, no backoff, no retry classification |

`OPEN` (decide deliberately): port the non-sync rate-limit gap as-is, or add a
courtesy backoff/classification for search/discover/enrich 429s. Lean: classify
429 distinctly and back off; hammering the catalog API is how apps get blocked.

---

## 9. TUI integration points (cross-ref)

| Concern | Where |
|---|---|
| Connect modal + cancel wake | 04 §4.6, 05 settings connect |
| Debounced push after status/play | 05 action-sync ROD-291 |
| Launch pull-refresh | 04 bootstrap |
| Sync master switch + provider row | 05 settings |
| Token survives reloadAuth during flush | 05 `reloadAuth retires…` → Arc/owned token |

---

## 10. What not to port blindly

| zigoku | sabigoku |
|---|---|
| Client id 43536 hardcoded forever | Own AniList app; build-time or config |
| ZON required | Any safe format; 0600 + total load remain |
| `zigoku` path segment | `sabigoku` |
| CLI provider search as primary UX | TUI + AniList catalog |
| Push keyed on provider row | Show-keyed dirty set |

---

## 11. Open questions

| ID | Question | Lean |
|---|---|---|
| O1 | Config/auth format (ZON vs TOML vs JSON) | TOML or JSON; decide in 08 |
| O2 | Ship non-TUI query CLI in M1 | Defer |
| O3 | Auto-import unmatched AniList list entries | **Decided (ROD-467):** yes for the WATCHING/REPEATING slice, add-only; other statuses count-only |
| O4 | AniList app registration for sabigoku | Blocker before real login ships |

---

## 12. Primary sources (freeze)

- `src/paths.zig`, `src/config.zig`, `src/auth.zig`
- `src/login.zig`, `src/login_loopback.zig`
- `src/sync.zig` (+ store dirty/reconcile APIs)
- `src/updatecheck.zig`, `src/update.zig`
- `src/main.zig` (subcommand dispatch, sync order)
- Tests in those files (auth control bytes, reconcile table, push 429/401)

---

## 13. Adversarial checklist (ROD-430)

- [ ] Auth file 0600 and separate from config
- [ ] Verify-before-persist on every login path
- [ ] Loopback CSRF state gate; cancel does not post
- [ ] Relay hash→query script preserved or re-proven
- [ ] Pull-then-push never inverted on first sync
- [ ] Progress max + status conflict rules exact
- [ ] Dirty set is engaged library shows with anilist_id only
- [ ] Update check silent on failure; TTL + future-dated cache
- [ ] Subcommands not eaten as query words after positionals
