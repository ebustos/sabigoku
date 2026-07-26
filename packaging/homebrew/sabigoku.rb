# url and sha256 are rewritten by the brew-publish job in release.yml at tag
# time. The values committed here are a working recipe for the last release, so
# `brew install --formula` on this file works, not a record of what the tap
# currently carries. No `version` stanza: Homebrew scans it from the url, and a
# redundant stanza fails `brew audit --strict`.
class Sabigoku < Formula
  desc "Terminal anime browser and player"
  homepage "https://github.com/vantroy/sabigoku"
  url "https://github.com/vantroy/sabigoku/releases/download/v0.1.0/sabigoku-v0.1.0-aarch64-apple-darwin.tar.gz"
  sha256 "84839431fb85c0ba7cfe98ad51586892fc09574dbf2e5be4b6db6427221221ce"
  license "GPL-3.0-or-later"

  # Binary formula over the release tarball, arm64-only since ROD-496 cut the
  # Intel macOS artifact. Intel Macs and Linux are served by cargo install, the
  # installer and the AUR package; this formula must never claim an arch the
  # release does not ship.
  depends_on :macos
  depends_on arch: :arm64
  # Playback shells out to mpv at runtime; nothing links it.
  depends_on "mpv"

  def install
    bin.install "sabigoku"
    prefix.install "LICENSE"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/sabigoku --version")
  end
end
