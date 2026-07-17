# 03 · Providers and resolve

| Field | Value |
|---|---|
| Status | `stub` |
| Ticket | ROD-424 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Priority | **Fill early** (play pipeline) |

## Purpose

How shows become streams: `SourceProvider` surface, registry order, preferred vs
pin, binding tiers, absences/routes, resolve walk, fallback/demote, play-error
taxonomy, and enough provider protocol detail to reimplement + golden-test.

## Outline (to fill)

- [ ] `SourceProvider` methods and semantics (`search`, `canonicalKey`, `episodes`, `resolve`, `coverRequest`)
- [ ] Registry: primary, preferred, ordered walk, `search_page_size`
- [ ] Pins / absences / routes (persistence + runtime)
- [ ] Binding tiers A / B / C (names and when each fires)
- [ ] Preferred-provider re-route on open (ROD-398) vs pin supremacy
- [ ] Resolve walk and fallback advance
- [ ] Resume demote contracts (stale Tier-C force, etc.)
- [ ] Play-error taxonomy (align with DESIGN toast matrix)
- [ ] Per-provider notes: allanime crypto, senshi, megaplay, jikan/http helpers, HLS
- [ ] AniSkip integration touchpoint
- [ ] Golden vectors / spike_stream linkage

## Primary sources

- `src/source.zig`
- `src/tui/resolve.zig`, `src/tui/resolve_state.zig`
- `src/resolver.zig`
- `src/providers/*`
- `src/player.zig`
- `src/aniskip.zig`
- DESIGN toast matrix (§4.10 in zigoku DESIGN / sabigoku DESIGN)

## Open questions

- `OPEN`: which providers ship day-one in sabigoku vs phased parity.
