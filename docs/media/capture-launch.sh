#!/usr/bin/env bash
# capture-launch.sh: launch sabigoku for media capture (ROD-464, ported from
# zigoku ROD-255).
#
# Uses your REAL data store ($XDG_DATA_HOME/sabigoku) so the app boots into your
# actual watchlist, and the REAL cover cache so art paints without refetching.
# Safe for the capture beats, by construction:
#   - mpv is STUBBED: the play key opens no player, creates no IPC socket, so no
#     resume/history write ever fires.
#   - the beats are watchlist-READ-ONLY: search + navigation only. The runner
#     lints out every mutating key before it boots anything.
#   - XDG_CONFIG_HOME is isolated by the runner: no auth.toml, so AniList sync
#     never starts; the Settings tour's palette save lands in a throwaway.
#   NEVER add store-writing keys to a beats file: P (plan), w/p/c/x (status),
#   r (recompute), u (undo), v (provider pin), X (delete), or a bare Enter in a
#   focused detail (plays). Those DO write.
#
# Usage (from repo root): ./docs/media/capture-launch.sh [sabigoku args...]
# Requires a built binary at ./target/release/sabigoku (cargo build --release).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="$ROOT/target/release/sabigoku"
[ -x "$BIN" ] || { echo "no binary at $BIN; run 'cargo build --release' first" >&2; exit 1; }

# Stub mpv on a throwaway PATH dir so the play key never opens a real player.
STUB="$(mktemp -d)"
trap 'rm -rf "$STUB"' EXIT
cat > "$STUB/mpv" <<'MPV'
#!/usr/bin/env bash
exit 0
MPV
chmod +x "$STUB/mpv"

export PATH="$STUB:$PATH"
export COLORTERM=truecolor

"$BIN" "$@" 2>/dev/null
