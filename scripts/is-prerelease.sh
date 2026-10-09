#!/usr/bin/env bash
# Prints `true` when a version is a prerelease and `false` when it is not.
#
# A prerelease ships only where someone has to ask for it: npm's `next`
# dist-tag, the exact container tag, and a GitHub Release flagged as a
# prerelease. It is kept off the surfaces that move people who never asked,
# which are the Homebrew tap, the MCP registry, the site and the GitHub
# Release that `releases/latest` resolves to. Every one of those decisions
# reads this script, so the release workflow, the recovery workflows and
# npm-dist-tag.sh cannot disagree about which tags count.
#
# The version is checked first, because the rule is only sound on a real
# version: `1.2.0-` has a hyphen and is not a prerelease of anything.
#
# Usage:
#   scripts/is-prerelease.sh 1.2.0        # false
#   scripts/is-prerelease.sh 1.2.0-rc.1   # true
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
VERSION="${1:-}"

if [ -z "$VERSION" ]; then
  echo "usage: $0 <version>" >&2
  exit 1
fi
"$SCRIPT_DIR/is-semver.sh" "$VERSION"

case "$VERSION" in
  *-*) echo true ;;
  *)   echo false ;;
esac
