# sabigoku Port Bible

Runtime, data, and scar tissue for rewriting [zigoku](https://github.com/vantroy/zigoku)
in Rust. This is not a second design novel: UI, focus, layout, and data-availability
rendering live in [`DESIGN.md`](DESIGN.md). M0 stack experiments live in
[`SPIKES.md`](SPIKES.md). Everything here answers **how the machine stays correct**.

Tracked as **[ROD-420](http://fern.comet-ruler.ts.net)** (parent) and children ROD-421…430.

---

## Source freeze

| Field | Value |
|---|---|
| **Repo** | `~/src/zig/zigoku` (upstream: vantroy/zigoku) |
| **Rev** | `083abd3f2af6be5a372414c6b291045c926402cd` |
| **Short** | `083abd3` |
| **Describe** | `v0.4.6-3-g083abd3` |
| **Tip commit** | Merge rod-419: heal stale episode totals for airing shows (ROD-419) |
| **Frozen at** | 2026-07-17 |

All chapter claims are relative to this rev unless a section explicitly notes an
**upstream delta**. Later zigoku fixes either:

1. land here as an amendment (update freeze or add a dated delta note), or
2. sit in [`docs/port/07-bug-ledger.md`](docs/port/07-bug-ledger.md) as
   "won't port yet" / "fix in Rust instead."

Do not silently rewrite chapters against a moving `master`.

```bash
# verify the freeze tree
git -C ~/src/zig/zigoku rev-parse HEAD
# expect: 083abd3f2af6be5a372414c6b291045c926402cd
```

---

## What this bible is for

A cold implementer (human or agent) should be able to rebuild a module from the
relevant chapter plus `DESIGN.md` **without** reading zigoku source, and without
reintroducing a bug already named in the bug ledger.

| Doc set | Owns |
|---|---|
| `DESIGN.md` | Terminal Ghost, layout, focus, toast matrix, data-reality rendering |
| `SPIKES.md` | M0 risk spikes and language ledger |
| `PORT.md` + `docs/port/*` | Modules, schema, providers/resolve, TUI runtime, behavior contracts, auth/sync/cli, scars, Rust mapping |

---

## Non-goals

- Re-document palette, layout grammar, or focus model (link `DESIGN.md`)
- Exhaustive ROD-ticket narrative archaeology past diminishing returns
- Copying Zig idioms as product requirements (translate **intent** → Rust shapes)
- Implementing the app (docs only; M1+ tracks separately)
- Perfect provider reverse-engineering diaries (enough to reimplement + golden-test)

---

## Reading order

1. This file (`PORT.md`) — freeze, map, status
2. [`docs/port/01-modules.md`](docs/port/01-modules.md) — boxes and arrows
3. [`docs/port/02-domain-and-sqlite.md`](docs/port/02-domain-and-sqlite.md) — types + schema
4. [`docs/port/03-providers-and-resolve.md`](docs/port/03-providers-and-resolve.md) — play pipeline
5. [`docs/port/04-tui-runtime.md`](docs/port/04-tui-runtime.md) — loop, workers, ownership intent
6. [`docs/port/05-behavior-contracts.md`](docs/port/05-behavior-contracts.md) — test-distilled must/must-not
7. [`docs/port/06-auth-sync-cli.md`](docs/port/06-auth-sync-cli.md) — OAuth, sync, paths, config
8. [`docs/port/07-bug-ledger.md`](docs/port/07-bug-ledger.md) — scars and clone-vs-fix
9. [`docs/port/08-rust-mapping.md`](docs/port/08-rust-mapping.md) — thin Zig→Rust notes (amend in M1)

Fill order (authoring) is not the same as reading order. Prefer:
**freeze → 02 → 03 → 05 inventory → 04 → 01 polish → 06/07 → 08 last.**

---

## Chapter status

Status values: `stub` → `draft` → `review-stable`.

A chapter becomes `review-stable` only after an adversarial pass
(cross-agent cold read, ROD-430) stops finding holes that would mislead an implementer.

| Chapter | Ticket | Status |
|---|---|---|
| `PORT.md` (this file) | ROD-421 | draft |
| [`01-modules.md`](docs/port/01-modules.md) | ROD-422 | draft |
| [`02-domain-and-sqlite.md`](docs/port/02-domain-and-sqlite.md) | ROD-423 | draft |
| [`03-providers-and-resolve.md`](docs/port/03-providers-and-resolve.md) | ROD-424 | draft |
| [`04-tui-runtime.md`](docs/port/04-tui-runtime.md) | ROD-425 | draft |
| [`05-behavior-contracts.md`](docs/port/05-behavior-contracts.md) | ROD-426 | draft |
| [`06-auth-sync-cli.md`](docs/port/06-auth-sync-cli.md) | ROD-427 | draft |
| [`07-bug-ledger.md`](docs/port/07-bug-ledger.md) | ROD-428 | stub |
| [`08-rust-mapping.md`](docs/port/08-rust-mapping.md) | ROD-429 | stub |
| Adversarial loop | ROD-430 | process (not a doc) |

---

## Authoring rules

1. **Code and tests first, memory never.** Cite zigoku paths and test names under the freeze rev.
2. **Mark holes.** Prefer `UNVERIFIED` / `OPEN` over confident wrong.
3. **Intent over Zig.** "Worker frees whole allocation" → "own the full buffer; no partial free of a shortened view."
4. **No DESIGN paste.** Link section numbers; do not restate Terminal Ghost.
5. **One freeze.** Deltas are explicit, dated, and small.
6. **Tests as citations.** `app_test: "setHistory follows the focused show…"` beats a vague paragraph.

### Annotation tags

| Tag | Meaning |
|---|---|
| `UNVERIFIED` | Written from inspection; needs a test cite or second read |
| `OPEN` | Decision or fact gap; adversarial fodder |
| `ZIG-SHAPE` | Zig-specific mechanism; translate before implementing in Rust |
| `CLONE` | Reproduce behavior as-is in sabigoku |
| `FIX-IN-RUST` | Known wart; do better in the port (also list in 07) |

---

## Primary sources (zigoku @ freeze)

| Source | Role |
|---|---|
| `src/store.zig` | Schema, migrations, concurrent open |
| `src/source.zig` | Provider vtable + registry |
| `src/tui/resolve.zig`, `src/tui/workers.zig`, `src/tui/app.zig` | Resolve walk, workers, app state |
| `src/tui/app_test.zig` | Behavioral contracts (~321 tests) |
| `src/providers/*`, `src/anilist.zig`, `src/sync.zig`, `src/auth.zig`, `src/login*.zig` | Edges |
| `DESIGN.md`, `CHANGELOG.md` | UI truth + user-facing scars |
| Inline contract comments | Landmines DESIGN never spells out |

Rough scale at freeze: ~27k production LOC in `src/`, ~754 tests, product at
`v0.4.6` plus a few post-tag commits through ROD-419.

---

## Milestone map (after the bible)

| Milestone | Meaning |
|---|---|
| **M0** | Done. Spikes in this repo (`SPIKES.md`) |
| **Bible** | ROD-420: this tree `review-stable` end-to-end |
| **M1** | Collapse spikes into real modules behind traits/interfaces |
| **M2+** | Feature parity slices driven by chapter contracts + `DESIGN.md` |

Implementation tickets are separate. Closing ROD-420 does not start M1 by itself;
it only removes the excuse to re-learn zigoku under panic.

---

## Adversarial review (ROD-430)

**Plane workflow for chapter tickets (ROD-421…429):**

| When | State |
|---|---|
| Not started | Backlog / Todo |
| Being written | In Progress |
| Filled enough for review | **In Review** (hold; do not Done yet) |
| After adversarial batch pass + amends | Done + chapter status `review-stable` in this file |

Fill all chapters to **In Review** first, then run the batch adversarial pass
(ROD-430) against the set. Parent ROD-420 stays In Progress until the batch is clear.

Per-chapter cold-read steps (when the batch runs, or sooner if needed):

1. Author commits a `draft` and moves the child ticket to **In Review**.
2. Reviewer cold-reads **without** re-opening zigoku first.
3. Attack list: missing invariant, `ZIG-SHAPE` smuggled as requirement, known bug
   unnamed, test contract uncaptured, ambiguity an implementer would coin-flip.
4. Author amends or cites source.
5. Promote chapter status to `review-stable` and ticket to **Done** only when
   implementer-misleading holes are gone.

---

## Chapter index

| # | File | Captures |
|---|---|---|
| 01 | [modules](docs/port/01-modules.md) | Module map + data flow |
| 02 | [domain-and-sqlite](docs/port/02-domain-and-sqlite.md) | Domain types, tables, migrations |
| 03 | [providers-and-resolve](docs/port/03-providers-and-resolve.md) | Registry, tiers, resolve walk, play errors |
| 04 | [tui-runtime](docs/port/04-tui-runtime.md) | Event loop, workers, cancel, cover pump |
| 05 | [behavior-contracts](docs/port/05-behavior-contracts.md) | Distilled must/must-not from tests |
| 06 | [auth-sync-cli](docs/port/06-auth-sync-cli.md) | OAuth, sync, update check, paths, config |
| 07 | [bug-ledger](docs/port/07-bug-ledger.md) | Scars, known issues, clone vs fix |
| 08 | [rust-mapping](docs/port/08-rust-mapping.md) | Thin Zig→Rust notes |
