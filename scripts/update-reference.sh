#!/bin/sh
# Refreshes the vendored copy of alea's reference from a local checkout.
# Usage: scripts/update-reference.sh ../alea
set -eu
cp "${1:?path to an alea checkout}/docs/reference.md" "$(dirname "$0")/../assets/reference.md"
echo "assets/reference.md updated; run cargo test to check it still parses."
