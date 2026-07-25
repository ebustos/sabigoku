# 08 · Rust mapping

| Field | Value |
|---|---|
| Status | `review-stable` |
| Ticket | ROD-429 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Note | **Keep thin.** Amend as M1 teaches. Do not over-speculate. |
| Spikes | [`SPIKES.md`](../../SPIKES.md) is the measured ledger; this chapter is the **default stack** for M1+ |

## Purpose

Translate **intent** from zigoku into Rust shapes so implementers do not re-import
Zig arenas, vtables, or ZON as product requirements. Product law lives in 02–07 and
DESIGN; this file only answers "what do we reach for in the crate graph?"

---

## 1. Default stack (M1 lean)

| Concern | Choice | Spike / chapter |
|---|---|---|
| HTTP + JSON | `reqwest` (blocking OK for workers) + `serde` / `serde_json` | SPIKES http; 04 workers |
| TLS | rustls via reqwest features (no system OpenSSL) | SPIKES |
| SQLite | `rusqlite` **bundled** | SPIKES sqlite; 02 schema |
| Threads + channel | `std::thread` + `std::sync::mpsc` (or `crossbeam-channel` if needed) | SPIKES concurrency; **04** default |
| Crypto (allanime) | `aes-gcm` + `sha2` + `base64` | SPIKES stream; 03 |
| Process / mpv | `std::process::Command` | SPIKES mpv; 03 |
| TUI | **ratatui** + **crossterm** | DESIGN |
| Cover images | `ratatui-image` (Kitty + halfblocks), **spiked ROD-417**; per-image `ThreadProtocol` request channels (ids are per-instance) | DESIGN §9.3/§11.2; SPIKES §6 |
| Config / auth files | **TOML** lean (`toml` + serde) or JSON; not ZON | 06 O1 |
| Errors | `thiserror` for libraries; `anyhow` only at binary edges if useful | — |
| Logging | `log` facade + `flexi_logger` file sink (ROD-450) | 06 debug flag |
| Semver (update check) | small pure helper or `semver` crate | 06 |

**Do not** default the whole app to a multi-thread tokio runtime just because
reqwest pulls one in transitively. Workers may use `reqwest::blocking` on their
own threads. Decided with the M1 cut: threads + mpsc (04 O1 closed); revisit
only on measured pain.

---

## 2. Ownership and concurrency

| Zig | Rust |
|---|---|
| GPA dupe / arena for worker payloads | `String`, `Vec<T>`, `Bytes`; move into task |
| Cross-thread slices with oral free rules | `Send` types only; no shared `&mut App` |
| `ThreadDrain` begin/finish | `AtomicUsize` / `WaitGroup` / runtime `Drop` guard (04 §5) |
| Keep-check string ids | same, or `AtomicU64` generation (04 §6) |
| History arena borrow into App | `Vec<Show>` / `Arc<[Show]>` swap on load |
| Token survives reload mid-flush | `Arc<str>` or owned `Auth` clone (05/06) |

**Footgun:** forgetting to drop the last `Sender` leaves `Receiver` blocked forever
(SPIKES concurrency). Document close protocol next to the event loop.

**Store access lean (01 §6):** UI thread performs store writes; workers return data
in events. If a worker must touch SQLite later, serialize behind a single
`Mutex<Connection>` and re-document here.

---

## 3. Domain and providers

| Zig | Rust |
|---|---|
| `SourceProvider` fat pointer vtable | `trait StreamProvider: Send + Sync` **or** enum of three providers (N small → enum is fine, 03 O2) |
| `Registry` slice + ordered iter | `Registry { providers: Vec<…> }` + `ordered(pref)` iterator |
| `domain.Anime` mega-struct | Split **Show** (AniList id + enrichment + user) vs **Binding** vs search DTOs (02) |
| Comptime `@tagName` persistence | `serde` string enums or explicit `as_str` / `from_str` |
| `anyerror` everywhere | Typed errors per layer; map to toast classes at TUI boundary (03 §7) |

---

## 4. Persistence

| Zig | Rust |
|---|---|
| Hand C API | `rusqlite` `params!`, `query_map`, transactions |
| `PRAGMA user_version` ladder | Same idea; one `BEGIN IMMEDIATE` migration txn (02) |
| New schema | **02** tables (`show`, `catalog_cache`, …); no zigoku v1–18 port |
| ZON config/auth | serde file format; auth file mode **0o600** on Unix (06) |

Migrations live in `store`; no `provider_migrate` zigoku re-key theater.

---

## 5. TUI

| Zig | Rust |
|---|---|
| libvaxis Loop + Cell | crossterm event stream + ratatui `Frame` / `Buffer` |
| tick mutates; draw pure | same split (04) |
| Kitty image widget | `ratatui-image`, spiked (ROD-417); half-block fallback per DESIGN |
| `App` megastruct | modules per 01 §3.1; avoid one 3k-line `app.rs` if possible |
| Input modes | enum + dispatch table (05/06) |

Runtime: **04** binds threads+channel default; DESIGN §9.8 is satisfied by that lean.

---

## 6. Error and user copy

- Library crates: structured errors (`thiserror`).
- TUI maps to DESIGN toast strings (03 taxonomy; 05 play/episodes tests).
- Soft network failures: toast + degrade, not panic.
- Sync: summary flags, not unwinding (06).

Do not use stringly `@errorName` as the only API; map known classes, generic fallback
last (zigoku `failureClassCopy` pattern).

---

## 7. Anti-patterns

| Don't | Do |
|---|---|
| Re-hand-roll HTTP retry/channel "like zigoku" | Use reqwest + std mpsc; put policy in **our** code only |
| `async fn` everything + `block_on` in the UI thread | Workers on threads; UI stays sync with ratatui |
| Provider-primary tables "because zigoku tests" | 02 identity |
| Mega-`App` with no modules | 01 slices |
| Skip 05 contracts for speed | Contracts are the regression spine |
| Unbounded `unwrap` on network | Typed errors + toast |
| Tokio multi-runtime / nested runtimes | One strategy; blocking client on worker threads is OK |
| C FFI for sqlite "for purity" | rusqlite bundled until a real gap appears |

---

## 8. Crate graph discipline

- Prefer **few** direct deps; add a crate when it deletes a class of bugs or a page of code.
- Pin versions in `Cargo.toml` / lockfile; upgrade deliberately.
- First-compile cost is accepted (SPIKES tax); keep CI warm caches if painful.
- Feature-gate optional cover backends if image stack gets heavy.

---

## 9. Test mapping

| zigoku | sabigoku |
|---|---|
| `app_test.zig` mega-file | Modules mirroring **05** sections |
| Store migrate tests | `#[test]` + in-memory rusqlite |
| Resolver pure matchers | pure unit tests, no network |
| Spikes golden crypto | keep offline vectors in tree |
| Live provider smoke | optional `#[ignore]` or feature, not default CI |

---

## 10. Amend log

| Date | Change |
|---|---|
| 2026-07-17 | Initial lean from SPIKES + chapters 01–07 |
| 2026-07-17 | ROD-430 round 1: verified against SPIKES/Cargo.toml/source; zero factual corrections needed. This chapter now **owns** the config/auth format decision (06 O1): lean TOML, decide at M1 start |
| 2026-07-18 | ROD-435 ratifications: 03 O2 settled as `dyn StreamProvider` trait objects. Non-sync AniList 429 gets a distinct `CatalogError::RateLimited` (no client retry; caller policy) instead of freeze's collapse-to-no-answer (06 §8b OPEN). `strip_controls` widened beyond freeze C0+DEL to C1 + bidi controls + zero-width. Redirects disabled on the catalog client. Search query carries `pageInfo{hasNextPage}` (Browse is AniList-fed here, provider-fed at freeze) |
| 2026-07-18 | ROD-436 ratifications: the resolve walk is a headless decision engine (pure fns over a `ResolveWorld` trait, actions returned as data) rather than freeze's App-coupled `resolve.zig`; the transport that executes actions is ROD-437. Walk origin is a 3-value `WalkOrigin` tag (ForcedPreferred / PinFlip / Fallback), replacing freeze's single `manual` bool, so the K-2 fix (03 §5.3) is a real behavioral split not a doc note. `cover_request` returns `Result` (freeze `coverRequest` errors on a bad ref). Provider transport carries a 10s API-POST / 20s long-tail-GET deadline split (freeze had no rail on the long tail beyond `deadline.zig`). SSRF `private_v6` widened past freeze to reject IPv4-compatible `::a.b.c.d` (backport owed). `SearchHit` widened with english/native titles + per-track counts to feed the tier-B/C scorers |
| 2026-07-19 | ROD-441 ratifications (recorded retroactively): senshi ported from the live tag **v0.4.7**, not the freeze `083abd3`. PRINCIPLE: the freeze governs provider ARCHITECTURE (seam/tiers/walk, 03 §1-7); provider PROTOCOL bytes (03 §8: query strings, CDN hosts, seeds) are live-site facts that legitimately track the live tag. senshi is plain REST keyed by mal_id, tier-A key, `cloaked_segments`, softsub sidecar |
| 2026-07-19 | ROD-439 ratifications (chunk 4): the route stamp mints the `show` identity row like the absence mark (02 §3.7 list extended; stamp-before-fetch must land for never-resolved shows, and an unstampable open falls back to a normal classify rather than forcing unstamped). zigoku's in-memory hot episode LRU (8 lists) is not ported; the sqlite `episode_cache` is the only listing cache. Walk-exhaust surfaces added past the freeze toast matrix: grid `no source` state + `no source found` toast (DESIGN §4.6/§4.10 amended). Provider-controlled episode labels are control-stripped at the one render edge where they meet the terminal (closes the ROD-441 backport check for sabigoku). Review reconciliation: DESIGN 4.10's pin `couldn't reach {provider}` row and 03 §5.1's `no match on {name}, pin kept` are DISTINCT events at the freeze (walk could not run vs walk ran and missed); 4.10 gained the flip-missed row and the hop-failed row's condition was clarified, and the unreachable `no canonical identity` pin row (retired `.direct` identity, 02) was dropped |
| 2026-07-19 | ROD-443/445 ratifications: the de-cloaking proxy + megaplay ported from the live tag **v0.4.8** (same protocol-bytes-track-live-tag principle as ROD-441). Proxy seam is a `Decloak` RAII guard (`proxy::engage`) rather than freeze's `proxy.play`-wraps-`player.play`, because sabigoku's mpv path (ROD-437) is not built yet; the guard's `Drop` brackets the proxy to the playback. `Arc<Proxy>` replaces freeze's `Gate` refcount + leak-not-free teardown (memory-safe by construction). Hardening past freeze: a client-facing per-socket read/write timeout + bounded request head (std affordances zigoku's Io lacked), and the final softsub `sub_url` is SSRF-guarded before mpv (freeze argv-vets only; both senshi + megaplay, backport owed). Deferred to ROD-439 (log sink): the `decloak` no-sync warn + fMP4 sniff, and megaplay's data-id-missing warn. Deferred to a hardening ticket: a per-playback token on the loopback path |

| 2026-07-23 | ROD-470: CLI dispatch skeleton (src/cli.rs, pure `parse` over argv + main.rs match). zigoku parse semantics ported exactly (subcommand = first non-flag positional, flags may precede, query-word demotion, `--quality v`/`=v`, single-dash words are query text, version outranks everything incl. bad flags). Exit law structural via ExitCode: everything 0 except the play path; interim query stub exits 2 per 06 §7.3 until ROD-473. DEVIATIONS: usage lists ALL subcommands incl. login/sync/update (zigoku's usage is query-only; ratified at scope), and `--paths` stays a sabigoku-only flag ranked just under `--version`. Logging policy completed: `logging::init_stderr` is the CLI sink (was only the TUI fallback), `--debug` ORs with SABIGOKU_DEBUG into both sinks (zigoku ROD-88 semantics), `logging::init` grew the flag parameter |
| 2026-07-23 | ROD-471: `sabigoku login` / `login --paste` wired to the ROD-448 OAuth machinery (main.rs `run_login_cli` + `cli::render_connect_result`; `open_browser` hoisted app.rs -> login.rs). Loopback bind/nonce failure falls back to paste (06 §4.3); persisted token (only) chains the full bootstrap sync via `run_sync_cli` (zigoku runSync after login; the ticket's "pull-only" phrasing was drift, source runs pull+push). CORRECTION riding along: `Loopback::serve` no longer terminates on a bad-state callback; it serves the fail page, fires an `on_bad_state` hook (CLI warns once, TUI logs), and keeps waiting (zigoku ROD-283 warn-once semantics; the 448 shape let any stray/forged local request abort a login in flight). Paste front ports zigoku's bare-eyJ acceptance via `normalize_paste`; 8 KiB stdin cap. PASTE ABORT RULE (ratified looser than zigoku, `cli::paste_line_usable`): empty EOF or a cap-length read with no newline aborts; a complete line WITHOUT a trailing newline is ACCEPTED, so a newline-less pipe (`printf '%s' "$url" | login --paste`) still logs in. zigoku aborts on any pre-newline EOF. DEVIATIONS: signed-in line drops zigoku's "(id N)" (ConnectResult carries only the name), and the AniList name is `strip_controls`'d before the raw CLI println (no ratatui backstop on that path); accept-loop death collapses into the NetworkError message instead of zigoku's fall-back-to-paste (rare; the listener socket dying mid-wait); punctuation ours. HARDENING (review): the TUI bad-state `log::warn` debounces once-per-worker (a forged-callback flood must not churn the rotating sink), and loopback drops a connection whose read deadline can't be set (the "no overall timeout" design leans on it) |
| 2026-07-23 | ROD-472: `sabigoku sync` wired to the ROD-448 engine (main.rs `run_sync_cli` + pure `cli::render_sync_summary`). Engine aligned to the 06 §5.2 law: only pull 401/429/store errors gate the push; a transport/decode miss sets `SyncSummary.pull_failed` and the push still runs (was: any fetch error terminal). `SyncOutcome` split (`PullUnauthorized`/`PullRateLimited` replace `PullFailed`, new `NoUserId`); summary carries the push work-list size for "pushed X of Y". CLI ignores `anilist_sync_enabled` (06 §5.5 asymmetry preserved) and exits 0 on every path incl. setup failures (zigoku runSync parity). DEVIATIONS: a token without a user id skips the whole run, not just the pull (login always stamps the id; zigoku pushed anyway); summary punctuation drops zigoku's dashes; new "imported N" line (ROD-467 auto-import postdates the freeze), and an import-only pull suppresses "already up to date" (zigoku suppresses on conflicts only); the engaged-but-unlinked listing has no counterpart (every library row keys on `anilist_id`) |
| 2026-07-23 | ROD-450: the log sink is the `log` facade + `flexi_logger` (rotating file under the data dir, `SABIGOKU_DEBUG` gate), settling the §2 "tracing or log" cell. Deviations from zigoku's `log.zig`: timestamps and size rotation (zigoku appends bare lines forever), foreign `log`-speaking crates capped at info in debug mode, and no O_NOFOLLOW on open (flexi_logger owns the open; the data dir is not world-writable). The ROD-443/445 deferred diagnostics land: decloak no-sync warn + fMP4 box sniff, megaplay data-id warn, and the ROD-300 always-on transport/status warns in provider http. Worker-panic trace rerouted from stderr (frame-punching) to the sink |
| 2026-07-22 | ROD-477: `show.progress_stamped_at` (schema v2) records when the frontier last moved, stamped by every progress writer (record_play/record_finish ratchet, recompute, raise-to-union, AniList adoption, import mint, manual status snap, undo restore) only when the value changes. `latest_resume` returns the freshest partial NEWER than the stamp. Root cause: ROD-439 rebuilt zigoku's frontier-anchored `resumeSeed` recency-anchored, reaching a state the freeze could not (stale partial behind the high-water); the freeze had no test for it and 05 §10.7's prose lost the anchoring qualifier. Deliberately NOT a re-port of `resumeSeed`: recency semantics fit the AniList-keyed multi-writer world (tablet syncs move the frontier with no local rows) and keep live rewatch checkpoints, which frontier anchoring discards |

| 2026-07-22 | ROD-478: History membership no longer depends on the app surviving to observe the play end. `record_engagement` (membership set-once + last_watched_at, nothing else) fires on the first meaningful position event; `adopt_orphaned_progress` at startup adopts progress rows under non-library shows (only plays write them, so an orphan proves an engagement the app died before stamping), timestamps from the rows. play_count/ratchet/status stay with the finish writers; a checkpoint is still not a play (02 §4b) |

| 2026-07-22 | ROD-449 ratifications: the prewarm walk (03 §6.5, 04 §7.6) is an event-driven UI-thread transport (`tui/prewarm.rs`) like the episode session, not freeze's self-contained worker thread: each candidate is one worker probe (`spawn_prewarm_probe` runs the whole tier-A-or-search chain), the 1.5s hop gap and 30s spacing ride the tick clock, and `prewarm_done` dissolves (the machine knows when its last probe settles). The 32-slot ring is kept (not 03 §6.5's sanctioned set lean). Cancel honors an in-flight probe's result (its network spend is sunk) instead of freeze's poll-between-hops. No `add_resolving` gate exists: P-save is a synchronous store write here, so the play gate is the launching window and the warm fires when mpv opens (first position), never during the play's own in-worker resolve. Probe classification mirrors the user walk, not zigoku's `resolveViaSearch`: absence only from an authoritative empty listing (03 §6.4's settled rule; a clean search miss learns nothing), and a dual-capability provider's empty tier-A listing does NOT fall through to search (zigoku ROD-367; the user walk here already picks Fetch or Search per provider, and prewarm must agree with the flip it serves). `count_hint` is passed to probes (freeze passed null): without it a listing-less provider would answer empty and false-absence itself on every warm. Review-gate hardening: a Found mint refuses a (provider, provider_id) pair another show already owns (bind_provider's steal-delete must never fire off a background probe's provider-claimed data; read errors fail the guard closed; the user-facing walk keeps freeze semantics, see backport.md). Teardown relies on drain alone, no cancel: the loop has stopped ticking, so no further probe can spawn. The on_result stale-token check is redundant under the single-flight guard and kept only as the subsystem token idiom |

| 2026-07-23 | ROD-447 ratification: the de-cloak proxy's loopback url carries a per-playback auth token (`/r.ts?t=<token>&u=`), 128 bits of `/dev/urandom` hex minted at `Proxy::start`; `serve`'s whole-prefix match is the gate, and a wrong or missing token 404s identically to an unknown path (no oracle). Closes the hardening item deferred at ROD-445 (only the process that started the playback can drive the relay; the SSRF guard already bounded it to public hosts). Not in zigoku at any tag (deviation past freeze). Nonce minting is shared with the OAuth CSRF nonce via the new `nonce` module. The prefix compare is early-exit, not constant-time: accepted, a timing probe against a 128-bit token over loopback TCP jitter is not a realistic adversary |
| 2026-07-23 | ROD-483: provider `log::debug!` instrumentation (first shipped debug call sites past main.rs, so the ROD-450 `sabigoku=debug` filter finally carries a provider record). STRUCTURAL DEVIATION from zigoku's per-provider debug set: zigoku logged each GET's error/HTTP-status per provider (allanime.zig@083abd3 351/421/425/546, deadline abort 402); sabigoku funnels every request through `providers/http.rs::fetch`, which already warns transport + non-accept status, so those lines collapse into ONE success-outcome debug at the shared seam (`{method} {url}: HTTP {status} ({n} bytes)`) covering all providers, and the deadline abort rides the reqwest-timeout transport warn. The three seam log sites (2 warns from ROD-450 + this debug) route the url through `log_url` (control-byte strip) because provider-controlled urls reach them unsanitized (allanime long-tail `link` is only SSRF-guarded, not `clean_arg`'d); a forged CR/LF must not inject a fake log record. Query string kept (ephemeral CDN params, never our token). Per-provider debug is trimmed to the decisions the http layer can't see: allanime fast4speed-direct + quality pick, senshi quality pick (both `quality=X picked Np from M variant(s)`, per-provider tag kept, NOT folded into pure `hls::select_variant`), megaplay net-new (no zigoku source) data-id sub/dub fork + ROD-377 softsub cue-upgrade. Standard: provider + stage + key ids + decision; URLs logged (ROD-450 precedent), never tokens/bodies. NO automated test for the filter path (ratified: unit-testing debug strings + a log-capture harness judged excessive for ~8 additive lines; verified by a manual `SABIGOKU_DEBUG=1` live fetch at smoke). The ROD-450 boundary note (filter proven, no call site exercised it) is closed by the http.rs anchor line |
| 2026-07-25 | ROD-491: the CLI query path binds to a SEARCH-CAPABLE provider, not merely the preferred one. RATIFIED DEVIATION from 03 s3.2 and 06 s7.3. zigoku at freeze (main.zig:178, "CLI does not walk the registry") takes `registry.preferred(cfg.preferred_provider)` and hard-fails when that provider cannot search; its own test asserts `preferred("")` is megaplay, and 03 s8.1 says megaplay has no tier C. So a STOCK zigoku install has a dead `zigoku <query>` too: this was faithful parity with an upstream defect, not port drift, and Rod's live zigoku only works because his config pins a searchable source. New `StreamProvider::supports_search()` (default true, false in megaplay) makes the structural fact structural instead of discovered by attempting; `ProviderRegistry::preferred_searchable(pref)` returns the first `ordered()` entry that can search. SCOPE OF THE WALK: search only. Whichever provider answers owns the whole run, because a provider id is meaningless on another (03 s3.2 binding rule is untouched). An explicit but incapable preference is walked past with a one-line note naming both. The old Unsupported/Search copy pointed users at `preferred_provider`; that advice now describes an unreachable state, so it is gone and the test asserting it inverted (`search_unsupported_no_longer_nudges_at_config`). TUI unaffected: it already walked via `ordered()`. Guard mutation-checked: replacing the `supports_search` filter with `.next()` fails 3 of the 4 new registry tests |

When M1 picks config format, image crate, or channel crate, add a row here rather
than rewriting product chapters.

---

## 11. Adversarial checklist (ROD-430)

- [ ] No requirement to port Zig allocators as product
- [ ] Thread+mpsc is the written default (not "undecided forever")
- [ ] AniList-first schema not contradicted
- [ ] Spikes cited for HTTP/sqlite/concurrency/crypto/mpv
- [ ] Anti-patterns name the real zigoku pain (HTTP, channels, ownership)
- [ ] Still thin enough to re-read in five minutes
