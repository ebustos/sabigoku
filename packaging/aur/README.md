# AUR package

`PKGBUILD` here is the source of truth. The `aur` job in `release.yml` renders it
for the tag, builds it in a pinned `archlinux:base-devel` container, and pushes
it to `ssh://aur@aur.archlinux.org/sabigoku.git`, where users reach it as
`paru -S sabigoku`.

It is a source build, which is what the plain package name conventionally means
on the AUR. `cargo install sabigoku` covers the same ground for anyone who
already has a toolchain; this exists so pacman owns the file.

## What the release job rewrites

`pkgver`, `pkgrel` and `sha256sums`. Everything else ships as committed, so a
change to `depends`, `check()` or the build flags has to land here and be
reviewed like any other code. The committed `pkgver` and checksum are a working
recipe for the last release rather than a record of what the AUR currently
carries, which means `makepkg` in this directory works without editing anything.

`.SRCINFO` is generated during the job and never committed here. The AUR rejects
a push without it, and a second copy in this repo is a second thing to drift.

## Prerequisites

`AUR_SSH_PRIVATE_KEY` in the repo secrets, holding a private key whose public
half is on the `vantroy` AUR account. The job pins the AUR host key, so a server
key rotation breaks the push rather than trusting a new one.

The key never shares a machine with the build. `aur-build` compiles and tests the
package and hands two text files to `aur-publish`, which installs no toolchain
and runs nothing from the crate graph.

## Doing it by hand

Local build and install, no AUR involved:

```sh
cd packaging/aur && makepkg -si
```

A packaging-only fix between releases needs a `pkgrel` bump, which the job never
does: it writes `pkgrel=1` for a new `pkgver` and skips when the AUR already has
that version. Push those by hand.
