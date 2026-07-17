# 05 · Behavior contracts

| Field | Value |
|---|---|
| Status | `stub` |
| Ticket | ROD-426 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Priority | **Highest adversarial heat** |

## Purpose

Distill `app_test.zig` (~321 tests) and other high-value tests into prose
contracts: **given / when / then / must not**, with test-name citations.

This chapter is the regression spine for the rewrite. Prefer inventory → group →
prose over freehand.

## Process

1. Inventory test names from `src/tui/app_test.zig` (and key store/provider tests).
2. Group by theme (history cursor, delete confirm, Discover, layout gates, …).
3. One subsection per theme with contracts + cites.
4. Adversarial pass: every ROD-tagged test name should map to a contract or an explicit "covered in 0N".

## Outline (themes to fill)

- [ ] Navigation and quit keys
- [ ] History cursor / `setHistory` anchoring / filters
- [ ] Hard-delete confirm (ROD-220)
- [ ] Status mutations, undo, recompute (ROD-193, ROD-189)
- [ ] Layout gates (60 / 100 cols, pane split)
- [ ] View switching (F-keys, B/H/D/S, search-mode guards)
- [ ] Discover axes, cache/stale, prefetch, covers, P/Enter binding rules
- [ ] Enrichment refresh preserve-user-state
- [ ] Title language resolution
- [ ] Resolve / preferred / demote (if tested here; else point to 03)
- [ ] Settings / connect surface contracts
- [ ] Esc chain

## Primary sources

- `src/tui/app_test.zig`
- `src/store.zig` tests (only what 02 does not already own)
- provider tests only when they encode cross-cutting product rules

## Inventory

_Paste grouped test-name lists here in the first fill pass._
