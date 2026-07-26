# Releasing

The ritual for cutting vX.Y.Z. A tag push publishes with no human gate
downstream (ROD-492), so every human decision happens before the tag exists.

## 1. Preconditions

- On master, clean tree, CI green on the tip commit.
- Pick the version. Pre-1.0: a breaking or platform-affecting change bumps
  minor, everything else bumps patch. Dropping an artifact CI never executed
  is a patch, since nothing that verifiably worked stops working; dropping a
  tested platform bumps minor. (Precedent: the Intel macOS cut shipped in
  0.1.1.)

## 2. Changelog

- `git cliff --unreleased` prints a grouped draft of everything since the last
  tag. It is an authoring aid, never the output: drafts carry commit scopes and
  ticket language that leak streaming source names, and the changelog is
  user-facing, where those never appear.
- Rewrite the draft as prose in a new `## [X.Y.Z] - YYYY-MM-DD` section: what a
  user gets, not a commit log. Add the compare link at the bottom of the file.
- `scripts/changelog-section.sh X.Y.Z` must print the section. release.yml runs
  that same script as its guard and publishes the output verbatim as the
  release body, so what it prints is what ships.

## 3. Version

- Bump `version` under `[package]` in Cargo.toml. Keep the line bare: the
  guard's parser does not strip inline comments.
- `cargo check` to land the bump in Cargo.lock.

## 4. Review

The release diff (changelog section, version bump, any ritual edits) goes
through the standard review gates like any other change, then commits as
`chore(release): vX.Y.Z`.

## 5. Tag

```sh
git push origin master
git tag vX.Y.Z
git push origin vX.Y.Z
```

- Push master first, so CI passes judgment before anything irreversible.
- The guard refuses a tag that disagrees with Cargo.toml or points at a commit
  whose changelog section is missing or empty.
- A suffixed tag (`v0.2.0-rc1`) publishes as a prerelease: GitHub release only,
  no crates.io, no AUR, no Homebrew.

## 6. What the workflow does

guard, then three builds (linux x86_64/aarch64 musl, macOS arm64), then the
GitHub release, then the independent channel jobs: crates.io publish, AUR
build then push (two jobs, so the key never shares a runner with crate build
scripts), and the Homebrew tap push.

## 7. Verify

- Release page: three tarballs plus sha256sums.txt, body identical to the
  changelog section.
- crates.io index carries X.Y.Z.
- AUR `pkgver` and tap formula url/sha256 bumped.
- The installer resolves the new version.
- On an arm64 Mac when one is at hand: `brew install vantroy/sabigoku/sabigoku`
  and `brew test`.

## Recovery

A failed channel job cannot be re-run against a fix on master: the run checks
out the tag's tree. Fix forward by hand (the AUR and Homebrew by-hand paths are
in `packaging/*/README.md`) or carry the fix into the next version. crates.io
is the one channel with no by-hand redo: a published version is permanent, and
its job skips cleanly on re-runs.
