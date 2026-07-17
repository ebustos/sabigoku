# 08 · Rust mapping

| Field | Value |
|---|---|
| Status | `stub` |
| Ticket | ROD-429 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Note | **Keep thin.** Amend as M1 teaches. Do not over-speculate. |

## Purpose

Zig → Rust translation notes and anti-patterns so implementers do not re-import
Zig shapes for no gain. Stack defaults already sketched in `SPIKES.md` and
`DESIGN.md` (ratatui + crossterm; runtime deliberately open).

## Outline (to fill)

- [ ] Ownership: arenas/GPA/dupe → owned types, `Arc`, explicit clones
- [ ] Provider vtable → trait objects or enum dispatch
- [ ] Config format (ZON → ?)
- [ ] HTTP/JSON: reqwest + serde (spike lessons)
- [ ] SQLite: rusqlite bundled (spike lessons)
- [ ] Concurrency: prefer `std::thread` + `mpsc` unless a measured need forces tokio-wide
- [ ] TUI: ratatui + crossterm; image path TBD
- [ ] Error types and user-facing mapping
- [ ] Anti-patterns: async spaghetti, mega-`App` with no modules, ignoring 05 contracts

## Primary sources

- [`SPIKES.md`](../../SPIKES.md)
- This bible's other chapters (intent statements tagged `ZIG-SHAPE`)
