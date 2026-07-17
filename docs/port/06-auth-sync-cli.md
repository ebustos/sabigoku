# 06 · Auth, sync, CLI

| Field | Value |
|---|---|
| Status | `draft` |
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
| Control bytes (`ch < 0x20`) in token → treat as signed-out | Header injection / HTTP assert abort |
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

**No overall timeout** on CLI wait (browser may be slow). Per-connection read deadline
so one stalled socket cannot wedge accept forever.

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

```
sync CLI / flush worker:
  pullAll → reconcile
  pushAll → SaveMediaListEntry for remaining dirty
```

TUI action flush (ROD-291) may be push-oriented after local edits; launch refresh is
pull-oriented. See 04/05 for when each arms.

### 5.3 Push (`pushAll`)

| | |
|---|---|
| Work list | Engaged, id-bearing, dirty rows only |
| Hidden/search-only | Not in push set (02: only library `show`) |
| Spacing | ~2s between row calls (AniList rate) |
| 429 | Sleep ~60s once, retry row; second 429 → stop run, rest stay dirty |
| 401 | Stop run immediately |
| Success | `markSynced` advances snapshot |
| No token / expired | No-op summary flags |
| Per-row other errors | Count failed, continue |

### 5.4 Pull + reconcile (`pullAll`)

Requires token, not expired, `user_id > 0`.

Fetch `MediaListCollection`. For each local reconcile candidate joined by
`anilist_id`:

**Pure merge (`reconcile`):**

| Field | Rule |
|---|---|
| **progress** | `max(local, remote)` always |
| **status** | Adopt remote only if local unmoved from base; if both moved differently → **keep local** + **conflict** (dirty for push) |
| **base** | Last synced pair; null base = first contact (treat base status as planning for "local moved") |

First contact examples (tests):

- Local planning + remote completed → adopt remote status, max progress.
- Local watching + remote dropped → keep local status if both "moved" from planning appropriately (see store tests).

After merge write:

- Re-baseline snapshot to **server truth** when remote moved (kept-local status stays dirty if conflict).
- **CAS / optimistic guard:** if local pair changed mid-flight, skip write (`contended`); retry next run.
- Unmatched remote ids: counted; **v1 does not auto-import** new AniList-only shows into library.
- Rewatch remote `REPEATING` → map to watching; progress still max.

### 5.5 Master switch

`anilist_sync_enabled = false` → TUI sync rail inert; **do not delete token**. CLI
`sync` should still respect or document override (lean: respect same flag if loaded).

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
| `--dub` | CLI play path translation |
| `--quality …` | CLI play quality |

### 7.3 Default binary behavior

| Invocation | Behavior |
|---|---|
| `sabigoku` | TUI |
| `sabigoku <query>…` | Optional non-TUI search→pick→mpv (zigoku had this). **Lean: defer** to post-M1 unless needed; TUI is the product (01 O3) |
| Bare flags only | TUI |

### 7.4 Exit / messaging

Sync/login print human summaries (signed-out, expired, rate limit, conflicts,
unmatched). Soft failures do not need nonzero exit if zigoku didn't; keep CLI
scriptable later if desired (`OPEN`).

---

## 8. Env vars

| Var | Role |
|---|---|
| XDG_CONFIG_HOME / XDG_DATA_HOME / XDG_CACHE_HOME / XDG_RUNTIME_DIR | Path bases |
| HOME | Fallbacks + collapse |
| COLORTERM | truecolor hint for TUI (04) |
| Debug env (zigoku `log.envDebug`) | Same idea: force debug without flag |

No token in env at freeze (file only). Keep it that way unless a test harness needs
injection.

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
| O3 | Auto-import unmatched AniList list entries | No (v1 unmatched count only) |
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
