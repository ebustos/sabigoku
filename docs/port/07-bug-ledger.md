# 07 · Bug ledger

| Field | Value |
|---|---|
| Status | `stub` |
| Ticket | ROD-428 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |

## Purpose

Scar tissue: CHANGELOG fixes worth not reintroducing, known issues at freeze,
and an explicit **CLONE** vs **FIX-IN-RUST** call for each.

## Outline (to fill)

- [ ] Process: mine `CHANGELOG.md` Fixed/Known Issues + high-signal ROD ids
- [ ] Table: id / symptom / root (if known) / disposition
- [ ] Known issues at freeze (resume-one-behind, empty grid under search-only preferred, …)
- [ ] Upstream deltas after freeze (dated list)
- [ ] Explicit non-ports (packaging, AUR, …) if any

## Disposition legend

| Tag | Meaning |
|---|---|
| `CLONE` | Reproduce the fixed behavior / still carry the limitation |
| `FIX-IN-RUST` | Do better in sabigoku; document intended new behavior |
| `N/A` | Zig-only or packaging-only; not product |

## Primary sources

- `CHANGELOG.md`
- DESIGN / app comments that name landmines
- Plane/ROD ids as index, not novels

## Ledger

_Empty until first fill pass._
