# 02 · Domain and SQLite

| Field | Value |
|---|---|
| Status | `stub` |
| Ticket | ROD-423 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |
| Priority | **Fill early** (schema is the spine other chapters name) |

## Purpose

Domain types and the SQLite contract: tables, `user_version` migration ladder,
concurrent open, enrichment field-set version, polluted search-cache rules.

## Outline (to fill)

- [ ] Domain types (`domain.zig`): Anime, Episode, progress, quality, translation, …
- [ ] Schema tables: `anime`, `episode_progress`, `episode_cache`, `app_meta`,
      `canonical_anime`, `provider_pins`, `provider_absences`, `provider_routes`, …
- [ ] `PRAGMA user_version` ladder (each step: what it does, irreversible notes)
- [ ] Concurrent open / migrate atomicity
- [ ] Enrichment freshness and field-set version
- [ ] Search-cache pollution hide rules
- [ ] Keying: provider rows vs canonical (AniList) rows
- [ ] Invariants that tests encode (cite test names)

## Primary sources

- `src/domain.zig`
- `src/store.zig` (+ its tests)
- `src/provider_migrate.zig` if still distinct at freeze

## Open questions

- `OPEN`: whether sabigoku reuses the same on-disk DB (compatibility) or a fresh schema with import. Decision belongs here or in 07/08 once weighed.
