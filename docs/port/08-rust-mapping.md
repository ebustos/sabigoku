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
| Cover images | `ratatui-image` / Kitty **unspiked** — treat as risk | DESIGN §11; 04 |
| Config / auth files | **TOML** lean (`toml` + serde) or JSON; not ZON | 06 O1 |
| Errors | `thiserror` for libraries; `anyhow` only at binary edges if useful | — |
| Logging | `tracing` or `log` + env filter | 06 debug flag |
| Semver (update check) | small pure helper or `semver` crate | 06 |

**Do not** default the whole app to a multi-thread tokio runtime just because
reqwest pulls one in transitively. Workers may use `reqwest::blocking` on their
own threads. Revisit only if measured pain (04 O1).

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
| Kitty image widget | spike before depending; half-block fallback per DESIGN |
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
