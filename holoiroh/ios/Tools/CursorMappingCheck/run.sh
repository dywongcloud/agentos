#!/usr/bin/env bash
#
# Compiles the REAL VideoViewportTransform + normalizedInVideo on macOS and checks that a touch
# on the live-share view drives the Mac cursor to the pixel the user is actually touching -- at
# every zoom level the pinch gesture can reach, not just at fit. Exits non-zero on regression.
# Used both locally and as a CI step.
#
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$DIR/../../Sources/HoloIrohApp"
OUT="$(mktemp -d)/cursor-mapping-check"

xcrun -sdk macosx swiftc -O \
  "$SRC/VideoViewportTransform.swift" \
  "$SRC/RemoteControl.swift" \
  "$DIR/main.swift" \
  -o "$OUT"

# The production call site is inside `#if canImport(UIKit)` and so is NOT built on macOS; the
# check reads these sources directly to catch a revert there.
export HOLOIROH_SWIFT_SOURCES="$SRC"
exec "$OUT"
