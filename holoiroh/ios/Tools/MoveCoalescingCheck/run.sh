#!/usr/bin/env bash
#
# Compiles the REAL MoveCoalescer on macOS and drives it through the exact stage-then-flush
# pattern FFIControlChannelSender uses, against a stand-in for the blocking bridge write.
# Proves the cursor follows the finger instead of replaying a backlog. Exits non-zero on
# regression. Used both locally and as a CI step.
#
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$DIR/../../Sources/HoloIrohApp"
OUT="$(mktemp -d)/move-coalescing-check"

xcrun -sdk macosx swiftc -O \
  "$SRC/MoveCoalescer.swift" \
  "$DIR/main.swift" \
  -o "$OUT"

# FFIControlChannelSender is iOS-only (it links the Rust bridge), so its call site is not built
# by this check; the source scan at the end catches a revert there.
export HOLOIROH_SWIFT_SOURCES="$SRC"
exec "$OUT"
