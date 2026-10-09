#!/usr/bin/env bash
# Prints the npm dist-tag a version publishes under: `next` for a prerelease,
# `latest` for anything else.
#
# npm refuses a bare publish of a prerelease ("You must specify a tag using
# --tag") and does not default one, and publishing a prerelease to `latest`
# would hand it to every `npm install dynoxide`. The rule lived in release.yml,
# npm.yml and npm/scripts/publish.sh at once, which is three places for it to
# drift; they call this, and this asks is-prerelease.sh, which the Homebrew,
# MCP registry and site decisions read as well.
#
# Usage:
#   scripts/npm-dist-tag.sh 1.2.0        # latest
#   scripts/npm-dist-tag.sh 1.2.0-rc.1   # next
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="${1:-}"

if [ -z "$VERSION" ]; then
  echo "usage: $0 <version>" >&2
  exit 1
fi

# An assignment rather than a test, so a malformed version stops the script
# under `set -e` instead of reading as "not a prerelease" and printing latest.
PRERELEASE=$("$SCRIPT_DIR/is-prerelease.sh" "$VERSION")

if [ "$PRERELEASE" = true ]; then
  echo next
else
  echo latest
fi
