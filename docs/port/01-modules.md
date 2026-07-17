# 01 · Modules and data flow

| Field | Value |
|---|---|
| Status | `stub` |
| Ticket | ROD-422 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Depends on | 02 (draft: AniList-first show identity), 03 (draft: providers/resolve); polish after those drafts exist |

## Purpose

Map the major modules and the end-to-end data flow:

`main → config/auth/store → source registry → TUI loop → workers → resolve → mpv → progress/sync`

UI layout and focus live in [`DESIGN.md`](../../DESIGN.md). This chapter only
names boxes, ownership boundaries, and call direction.

## Outline (to fill)

- [ ] Binary entry and subcommands (`main.zig`)
- [ ] Config + paths + auth token load
- [ ] Store open/migrate
- [ ] Provider registry construction
- [ ] TUI `App` ownership of state slices
- [ ] Worker pool / event channel (direction of messages)
- [ ] Resolve → player → progress writeback
- [ ] Sync and update-check as side paths
- [ ] Diagram (ascii or mermaid)

## Primary sources

- `src/main.zig`
- `src/tui/app.zig`
- `src/tui/workers.zig`
- `src/tui/event.zig`
- `src/source.zig`

## Open questions

- `OPEN`: final Rust crate split (single binary vs `sabigoku_*` libs) deferred to 08 / M1.
