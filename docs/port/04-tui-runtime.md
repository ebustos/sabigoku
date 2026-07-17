# 04 · TUI runtime

| Field | Value |
|---|---|
| Status | `stub` |
| Ticket | ROD-425 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Depends on | 03 (resolve walk named); DESIGN for views/focus |

## Purpose

Event loop, workers, cancel, cover pump, Discover axes, and ownership/lifetime
**intent** (not Zig arena mechanics). Map to a simple Rust model; leave tokio vs
threads open per DESIGN §9.8 unless this chapter forces a call.

## Outline (to fill)

- [ ] Main loop phases (input → drain events → render)
- [ ] Event types and who produces them
- [ ] Worker caps, spawn, cancel, join rules (what must never block the loop)
- [ ] Cover fetch pump (concurrency cap, in-flight room)
- [ ] Catalog cache upsert on Browse/Discover pages; detail card reads cache before network (see 02 §3.5)
- [ ] Discover multi-axis fan-out and feed retention
- [ ] Prewarm / episode fetch overlap rules
- [ ] String/buffer ownership across thread boundary (`ZIG-SHAPE` → Rust)
- [ ] Toast push rules that are runtime (not visual): drain on teardown, topic collapse
- [ ] Playback session lifecycle relative to the TUI

## Primary sources

- `src/tui/app.zig`
- `src/tui/workers.zig`
- `src/tui/event.zig`
- `src/tui/cover_state.zig`, `discover_covers.zig`, `prewarm_state.zig`
- `src/tui/playback_session.zig`
- DESIGN §7 / sabigoku DESIGN §9 (handoff + open runtime)

## Open questions

- `OPEN`: ratatui-image / Kitty path unspiked in sabigoku; note risk, do not fake a design.
