# Homebrew formula

`sabigoku.rb` here is the source of truth. The `brew-publish` job in
`release.yml` renders it for the tag and pushes it to
`vantroy/homebrew-sabigoku`, where users reach it as
`brew install vantroy/sabigoku/sabigoku`.

It is a binary formula over the published macOS arm64 tarball, the mac mirror
of the AUR package. The formula refuses other hosts rather than promising an
artifact the release does not ship: Linux has the installer, the AUR and
`cargo install sabigoku`; an Intel Mac has `cargo install` only, and
`install.sh` tells it so.

## What the release job rewrites

The version inside the `url` and the `sha256`. Everything else ships as
committed, so a change to dependencies, the install block or the test has to
land here and be reviewed like any other code. The committed values are a
working recipe for the last release rather than a record of what the tap
currently carries.

## Prerequisites

`HOMEBREW_TAP_DEPLOY_KEY` in the repo secrets, holding a private key whose
public half is a write deploy key on `vantroy/homebrew-sabigoku`. A deploy key
because the workflow's own `GITHUB_TOKEN` cannot push cross-repo, and a key
scoped to the tap alone is narrower than any PAT. The job pins the GitHub host
key, so a server key rotation breaks the push rather than trusting a new one.

## Doing it by hand

Local install on a Mac, no tap involved:

```sh
brew install --formula packaging/homebrew/sabigoku.rb
```

Full validation only works from the tap itself:

```sh
brew audit --strict --online vantroy/sabigoku/sabigoku
brew test vantroy/sabigoku/sabigoku
```

A packaging-only fix between releases needs a `revision` bump in the tap copy,
which the job never writes: it rewrites url and sha256 for a new version and
skips when the tap already matches. Push those by hand.
