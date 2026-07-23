#!/usr/bin/env bash
#
# Compiles the REAL ConnectionProfileRepository + ConnectionProfileStore (and
# their pure deps) on macOS and runs the conformance check. Exits non-zero if any
# always-present-default invariant regresses. Used both locally and as a CI step.
#
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$DIR/../../Sources/HoloIrohApp"
OUT="$(mktemp -d)/repo-conformance-check"

xcrun -sdk macosx swiftc -O \
  "$SRC/ConnectionProfileRepository.swift" \
  "$SRC/ConnectionProfileStore.swift" \
  "$SRC/PairingPhrase.swift" \
  "$SRC/PairingWordlist.swift" \
  "$DIR/main.swift" \
  -o "$OUT" -lsqlite3

exec "$OUT"
