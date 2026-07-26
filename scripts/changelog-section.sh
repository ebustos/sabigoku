#!/usr/bin/env bash
# Prints the CHANGELOG.md section for one version, failing if it is missing or blank.
#
#     scripts/changelog-section.sh 0.2.0 [changelog]
#
# release.yml calls this from both the guard job and the publish job. That is the
# whole point of it being a script: the body the guard validates and the body
# published as the release notes come from one extraction and cannot drift.
set -euo pipefail

version=${1:?usage: changelog-section.sh <version> [changelog]}
changelog=${2:-CHANGELOG.md}

# Errors go to stderr, never stdout: stdout is the section body, and both
# callers redirect it. On stdout the guard would discard its own error into
# /dev/null and publish would write it into the release notes. The runner parses
# ::error:: on either stream.
die() {
  echo "::error::$*" >&2
  exit 1
}

[ -f "$changelog" ] || die "no ${changelog} to read"

grep -qE "^## \[${version}\]" "$changelog" ||
  die "no '## [${version}]' section in ${changelog}; write it before tagging"

# A section ends at the next version heading or at the link-reference block,
# whichever comes first.
body=$(awk -v ver="$version" '
  $0 ~ "^## \\[" ver "\\]" { g = 1; next }
  g && /^## \[/            { exit }
  g && /^\[[^]]+\]:/       { exit }
  g                        { print }
' "$changelog")

[ -n "$(printf '%s' "$body" | tr -d '[:space:]')" ] ||
  die "section '## [${version}]' in ${changelog} is empty"

printf '%s\n' "$body"
