#!/bin/sh
# sabigoku installer: fetch the release tarball for this machine, verify it
# against the published checksums, and put the binary on your PATH.
#
#   curl -fsSL https://raw.githubusercontent.com/vantroy/sabigoku/master/install.sh | sh
#
# Knobs (all optional, via env):
#   SABIGOKU_VERSION  pin a release (0.2.0 or v0.2.0); default = latest
#   BINDIR            where the binary lands; default = ~/.local/bin
#   PREFIX            if set and BINDIR is not, BINDIR = $PREFIX/bin
#
# POSIX sh on purpose: this runs on whatever /bin/sh a fresh box ships.
set -eu

REPO="vantroy/sabigoku"
BIN="sabigoku"

say()  { printf '%s\n' "$*"; }
err()  { printf 'error: %s\n' "$1" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# One download tool, curl or wget.
if have curl; then
  dl()        { curl -fsSL "$1" -o "$2"; }
  dl_stdout() { curl -fsSL "$1"; }
  # /releases/latest redirects to /releases/tag/vX.Y.Z, so the tag can be read
  # off the landing URL. The API answers the same question but is rate limited
  # to 60/hour per IP unauthenticated, which is what breaks a piped installer
  # behind a shared NAT.
  latest_tag() {
    _url=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
           "https://github.com/${REPO}/releases/latest") || return 1
    case "$_url" in
      */releases/tag/*) printf '%s\n' "${_url##*/releases/tag/}" ;;
      *) return 1 ;;
    esac
  }
elif have wget; then
  dl()        { wget -qO "$2" "$1"; }
  dl_stdout() { wget -qO- "$1"; }
  latest_tag() { return 1; }
else
  err "need curl or wget to download; install one and retry"
fi

# Fallback for the redirect, and the only route wget has.
api_latest_tag() {
  dl_stdout "https://api.github.com/repos/${REPO}/releases/latest" |
    grep '"tag_name"' | head -1 |
    sed -E 's/.*"tag_name" *: *"([^"]+)".*/\1/'
}

# One checksum tool, GNU sha256sum or BSD/macOS shasum.
if have sha256sum; then
  sha256() { sha256sum "$1" | awk '{print $1}'; }
elif have shasum; then
  sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
else
  err "need sha256sum or shasum to verify the download; install one and retry"
fi

# Map this machine to a published target triple.
os=$(uname -s)
arch=$(uname -m)

case "$os" in
  Linux | Darwin) ;;
  *) err "unsupported OS '$os'; sabigoku publishes Linux and macOS builds only" ;;
esac

case "$arch" in
  x86_64 | amd64)  cpu="x86_64" ;;
  aarch64 | arm64) cpu="aarch64" ;;
  *) err "unsupported architecture '$arch'; sabigoku publishes x86_64 and aarch64 builds only" ;;
esac

# Rosetta 2 reports x86_64 on Apple Silicon, so uname alone cannot tell an
# emulated Apple Silicon Mac from a real Intel one. The former gets the native
# build; the latter gets nothing, because no Intel binary is published. Decide
# this from the target list and never from whether an asset exists: a stale
# release may still carry an Intel tarball.
if [ "$os" = "Darwin" ] && [ "$cpu" = "x86_64" ]; then
  if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || true)" = "1" ]; then
    cpu="aarch64"
  else
    err "Intel Macs have no published binary. Build from source instead:
  cargo install sabigoku
See https://github.com/${REPO}#install for the other options."
  fi
fi

case "$os" in
  Linux)  target="${cpu}-unknown-linux-musl" ;;
  Darwin) target="${cpu}-apple-darwin" ;;
esac

# Resolve the version.
if [ "${SABIGOKU_VERSION:-}" != "" ]; then
  ver="${SABIGOKU_VERSION#v}"
else
  tag=$(latest_tag) || tag=""
  [ -n "$tag" ] || tag=$(api_latest_tag) || tag=""
  ver="${tag#v}"
  [ -n "$ver" ] || err "could not resolve the latest release (rate-limited? set SABIGOKU_VERSION to pin one)"
fi

# The version lands in both a URL and a local path, so pin its shape before it
# reaches either. Anything with a slash, whitespace or a shell metacharacter is
# not a tag we publish.
case "$ver" in
  '' | *[!0-9A-Za-z.+-]*) err "invalid version '$ver'; expected a release like 0.2.0" ;;
esac

tarball="${BIN}-v${ver}-${target}.tar.gz"
base="https://github.com/${REPO}/releases/download/v${ver}"

if [ "${BINDIR:-}" != "" ]; then
  bindir="$BINDIR"
elif [ "${PREFIX:-}" != "" ]; then
  bindir="$PREFIX/bin"
else
  [ "${HOME:-}" != "" ] || err "HOME is unset; set BINDIR to choose an install dir"
  bindir="$HOME/.local/bin"
fi

say "sabigoku installer"
say "  target   ${target}"
say "  version  v${ver}"
say "  into     ${bindir}"

tmp=$(mktemp -d "${TMPDIR:-/tmp}/sabigoku-install.XXXXXX") || err "could not create a temp dir"
staged=""  # a half-written binary in $bindir, removed if we die before the rename
cleanup() {
  rm -rf "$tmp"
  if [ -n "$staged" ]; then rm -f "$staged"; fi
}
trap cleanup EXIT INT TERM

say "downloading ${tarball} ..."
dl "${base}/${tarball}" "$tmp/$tarball" ||
  err "download failed. Is v${ver} published for ${target}? See https://github.com/${REPO}/releases"
dl "${base}/sha256sums.txt" "$tmp/sha256sums.txt" ||
  err "could not fetch sha256sums.txt for v${ver}"

# Verify before unpacking. This is the point of the channel, not a nicety: a
# piped installer that skips it is worse than no installer.
want=$(awk -v f="$tarball" '$2 == f {print $1}' "$tmp/sha256sums.txt")
[ -n "$want" ] || err "no checksum for ${tarball} in sha256sums.txt; refusing to install unverified"
got=$(sha256 "$tmp/$tarball")
if [ "$want" != "$got" ]; then
  err "checksum mismatch for ${tarball}
  expected ${want}
  got      ${got}
refusing to install a tampered or corrupt download"
fi
say "checksum OK"

tar -xzf "$tmp/$tarball" -C "$tmp" || err "could not extract ${tarball}"
# The tarball is flat: the binary sits at the archive root beside LICENSE and
# README.md, with no version directory to descend into.
srcbin="$tmp/${BIN}"
[ -f "$srcbin" ] || err "extracted archive is missing the ${BIN} binary"

mkdir -p "$bindir" || err "could not create ${bindir}"

# Stage into the SAME dir, then rename over the target. rename() is atomic, so a
# reader never sees a half-written binary and a death mid-write leaves the old
# one intact. It also sidesteps ETXTBSY: a running executable cannot be written
# over, but it can be renamed onto, which is what will let `sabigoku update`
# replace the binary it is running from. Same-dir is required, since a
# cross-filesystem rename degrades to copy+unlink and loses both properties.
# mktemp creates the file O_EXCL under an unguessable name, so a co-tenant on a
# shared bindir cannot pre-plant a symlink for the fallback cp to write through.
staged=$(mktemp "$bindir/.${BIN}.new.XXXXXX") ||
  err "could not stage into ${bindir} (permission? set BINDIR to a writable dir)"
install -m 0755 "$srcbin" "$staged" 2>/dev/null ||
  { cp "$srcbin" "$staged" && chmod 0755 "$staged"; } ||
  err "could not write the staged binary in ${bindir} (permission? set BINDIR to a writable dir)"
mv -f "$staged" "$bindir/${BIN}" || err "could not move the staged binary into ${bindir}/${BIN}"
staged=""  # placed; the trap must not remove the installed binary

say ""
say "installed ${BIN} v${ver} to ${bindir}/${BIN}"

case ":$PATH:" in
  *":$bindir:"*) : ;;
  *) say ""
     say "note: ${bindir} is not on your PATH. Add it, e.g.:"
     say "  export PATH=\"${bindir}:\$PATH\"" ;;
esac

# A copy from another channel earlier on PATH shadows what we just installed,
# and the symptom is an install that appears to do nothing.
shadow=$(command -v "$BIN" 2>/dev/null || true)
if [ -n "$shadow" ] && [ "$shadow" != "$bindir/${BIN}" ]; then
  say ""
  say "note: another ${BIN} at '${shadow}' will run instead of this one."
  say "  remove it, or put ${bindir} ahead of it on your PATH."
fi

if ! have mpv; then
  say ""
  say "note: sabigoku plays through mpv, which isn't bundled. Install it:"
  say "  Linux:  your package manager (e.g. apt/pacman/dnf install mpv)"
  say "  macOS:  brew install mpv"
fi

say ""
say "run 'sabigoku --version' to check, or just 'sabigoku' to start."
