#!/usr/bin/env bash
# Installs mcp-publisher into the current directory at a pinned release, and
# refuses it unless the download matches the digest below. It runs in jobs that
# can mint an OIDC token for the MCP registry, so it takes a fixed, checked
# build rather than whatever `latest` points at on the day.
#
# The release path and the MCP recovery workflow both install it, so the pin
# lives here once rather than in two copies that could drift.
#
# To move to a newer release, change VERSION and SHA256 together. Take the
# digest for mcp-publisher_linux_amd64.tar.gz from that release's
# registry_<version>_checksums.txt.
#
# Usage:
#   scripts/install-mcp-publisher.sh
#
# MCP_PUBLISHER_BASE exists so the test can serve the download locally. It
# cannot loosen the check: whatever it serves still has to match SHA256.
set -euo pipefail

VERSION="1.8.1"
ASSET="mcp-publisher_linux_amd64.tar.gz"
SHA256="a06c9096dcb9727c13555b6be26c7effa707b01f06a4c561ba7a3635443cf2cc"

# The digest is for the linux amd64 build, which is what ubuntu-latest runs.
if [ "$(uname -s)" != "Linux" ] || [ "$(uname -m)" != "x86_64" ]; then
  echo "::error::The pinned mcp-publisher is the linux amd64 build, and this runner is $(uname -s) $(uname -m)" >&2
  exit 1
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

BASE="${MCP_PUBLISHER_BASE:-https://github.com/modelcontextprotocol/registry/releases/download}"
if ! curl -fsSL --retry 3 -o "$TMP/$ASSET" "$BASE/v$VERSION/$ASSET"; then
  echo "::error::could not download mcp-publisher $VERSION from $BASE" >&2
  exit 1
fi

if ! echo "$SHA256  $TMP/$ASSET" | sha256sum -c --quiet -; then
  echo "::error::mcp-publisher $VERSION does not match its pinned SHA-256, so it was not installed" >&2
  exit 1
fi

tar -xzf "$TMP/$ASSET" -C . mcp-publisher
echo "Installed mcp-publisher $VERSION ($SHA256)"
