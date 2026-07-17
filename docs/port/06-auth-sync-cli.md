# 06 · Auth, sync, CLI

| Field | Value |
|---|---|
| Status | `stub` |
| Ticket | ROD-427 |
| Freeze | zigoku `083abd3` (see [`PORT.md`](../../PORT.md)) |

## Purpose

OAuth loopback (byte-critical bits), token file permissions, AniList sync,
update check, paths, config keys, and CLI/subcommand surface.

## Outline (to fill)

- [ ] Paths layout (config, DB, cache, token)
- [ ] Config keys and defaults (incl. `palette`, preferred source, title language, …)
- [ ] Auth token format + `0600` atomic write
- [ ] Login loopback server: routes, redirect script constraints, cancel
- [ ] Sync push/pull semantics and conflict rules
- [ ] Update check TTL / clock skew
- [ ] CLI: default TUI, `login`, `sync`, other subcommands at freeze
- [ ] Env vars if any

## Primary sources

- `src/paths.zig`, `src/config.zig`, `src/auth.zig`
- `src/login.zig`, `src/login_loopback.zig`
- `src/sync.zig`
- `src/update.zig`, `src/updatecheck.zig`
- `src/main.zig` (subcommand dispatch)
