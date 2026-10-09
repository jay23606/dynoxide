#!/usr/bin/env bash
# Decides whether a release publishes the crate, and says why. Prints one of:
#
#   publish                     the crate version is not on crates.io yet
#   already-published           it is, and a crates.io version cannot be replaced
#   stable-crate-on-prerelease  the product version is a prerelease and the
#                               crate version is not
#
# and exits 0. Exits 1 when an argument is not a version or crates.io would not
# answer.
#
# The crate is on its own version stream, so a prerelease tag can sit on a
# commit whose Cargo.toml holds an unpublished stable version. Publishing it
# would ship rc code under a stable number for good, and the stable release
# would later find it taken and skip it. A prerelease crate on a prerelease
# tag publishes as normal.
#
# That case is settled before crates.io is asked, so an outage there cannot
# hold back the rest of an rc. release.yml and publish-crate.yml both read
# this, and act on the answer differently: the release carries on, the
# recovery workflow stops.
#
# Usage:
#   scripts/crate-publish-decision.sh dynoxide-rs 2.1.0 1.3.0-rc.1
#
# CURL, CRATES_API and RETRY_DELAY pass through to is-crate-published.sh.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CRATE="${1:-}"
CRATE_VERSION="${2:-}"
PRODUCT_VERSION="${3:-}"

if [ -z "$CRATE" ] || [ -z "$CRATE_VERSION" ] || [ -z "$PRODUCT_VERSION" ]; then
  echo "usage: $0 <crate> <crate-version> <product-version>" >&2
  exit 1
fi

# Assignments, so a version that is not one stops the script under `set -e`.
PRODUCT_PRERELEASE=$("$SCRIPT_DIR/is-prerelease.sh" "$PRODUCT_VERSION")
CRATE_PRERELEASE=$("$SCRIPT_DIR/is-prerelease.sh" "$CRATE_VERSION")

if [ "$PRODUCT_PRERELEASE" = true ] && [ "$CRATE_PRERELEASE" = false ]; then
  echo stable-crate-on-prerelease
  exit 0
fi

ALREADY=$("$SCRIPT_DIR/is-crate-published.sh" "$CRATE" "$CRATE_VERSION")

if [ "$ALREADY" = true ]; then
  echo already-published
else
  echo publish
fi
