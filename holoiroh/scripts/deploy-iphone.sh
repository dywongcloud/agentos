#!/usr/bin/env bash
#
# deploy-iphone.sh — one-command build + install + launch of the HoloIroh iOS app.
#
# Why this exists: the wireless CoreDevice tunnel is flaky and once wasted ~2.5
# hours of a session returning "No provider was found" (error 1011) on every
# install. This script builds once, then RETRIES the install until it actually
# lands — so shipping is never a gamble. A USB cable makes the tunnel reliable
# and the install instant; wireless works too, it just may take a few retries.
#
# Usage:
#   scripts/deploy-iphone.sh                 # build, install (retry), launch, show diagnostic
#   scripts/deploy-iphone.sh --skip-build    # reuse the last build, just (re)install
#   scripts/deploy-iphone.sh --no-launch     # install only
#   HOLOIROH_DEVICE_ID=<id> scripts/deploy-iphone.sh
#   HOLOIROH_INSTALL_RETRY_SECONDS=60 scripts/deploy-iphone.sh   # shorter retry window
#
set -euo pipefail

# ---- config (env-overridable) -------------------------------------------------
DEVICE_ID="${HOLOIROH_DEVICE_ID:-8F8FA990-17B6-5D7B-8105-7AD59C06F483}"
BUNDLE_ID="${HOLOIROH_BUNDLE_ID:-com.dylanwong.holoiroh}"
RETRY_SECONDS="${HOLOIROH_INSTALL_RETRY_SECONDS:-1800}"   # keep retrying up to 30 min
RETRY_INTERVAL="${HOLOIROH_INSTALL_INTERVAL:-15}"
NO_LAUNCH=0
SKIP_BUILD=0

while [ $# -gt 0 ]; do
  case "$1" in
    --no-launch) NO_LAUNCH=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --device) DEVICE_ID="$2"; shift ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown flag: $1" >&2; exit 2 ;;
  esac
  shift
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"   # holoiroh/
PROJ="$REPO_ROOT/ios-app/HoloIroh.xcodeproj"
DD="$REPO_ROOT/ios-app/build"
APP="$DD/Build/Products/Debug-iphoneos/HoloIroh.app"

log() { printf '\033[1;34m▸\033[0m %s\n' "$*"; }
ok()  { printf '\033[1;32m✓\033[0m %s\n' "$*"; }
err() { printf '\033[1;31m✗\033[0m %s\n' "$*" >&2; }

# ---- device selection: prefer a USB-attached (wired) device ------------------
# A USB-connected iPhone appears in `idevice_id -l` (libimobiledevice, USB-only);
# its wireless-vs-wired state doesn't change the CoreDevice id we install with,
# but a USB link makes the install tunnel reliable. If HOLOIROH_DEVICE_ID isn't
# pinned and exactly one device is paired, auto-pick it.
usb_present() { command -v idevice_id >/dev/null 2>&1 && [ -n "$(idevice_id -l 2>/dev/null)" ]; }

if [ "${HOLOIROH_DEVICE_ID:-}" = "" ]; then
  paired="$(xcrun devicectl list devices 2>/dev/null | grep -iE 'iPhone|iPad' | awk '{print $3}' | head -1 || true)"
  [ -n "$paired" ] && DEVICE_ID="$paired"
fi

if usb_present; then
  ok "USB device detected — install should land immediately."
else
  log "No USB device detected; using the wireless tunnel (retry-until-installed). Plug in a USB cable to make this instant."
fi
log "Target device: $DEVICE_ID"

# ---- build --------------------------------------------------------------------
if [ "$SKIP_BUILD" = 0 ]; then
  log "Building HoloIroh (Debug, device)…"
  xcodebuild -project "$PROJ" -scheme HoloIroh -configuration Debug \
    -destination 'generic/platform=iOS' -derivedDataPath "$DD" \
    -allowProvisioningUpdates build \
    | grep -E 'BUILD SUCCEEDED|BUILD FAILED| error:' | tail -20 || true
fi
[ -d "$APP" ] || { err "app not built at $APP (run without --skip-build)"; exit 1; }

# ---- install with retry-until-installed --------------------------------------
log "Installing to $DEVICE_ID (retrying up to ${RETRY_SECONDS}s)…"
deadline=$(( $(date +%s) + RETRY_SECONDS ))
attempt=0
while :; do
  attempt=$((attempt + 1))
  out="$(xcrun devicectl device install app --device "$DEVICE_ID" "$APP" 2>&1 || true)"
  if printf '%s' "$out" | grep -q "App installed"; then
    ok "installed (attempt $attempt)"
    break
  fi
  now=$(date +%s)
  if [ "$now" -ge "$deadline" ]; then
    err "install timed out after ${RETRY_SECONDS}s — the device tunnel never came up."
    err "Plug the iPhone in with a USB cable (and unlock it) and re-run; that bypasses the flaky wireless tunnel."
    printf '%s\n' "$out" | tail -3 >&2
    exit 1
  fi
  reason="$(printf '%s' "$out" | grep -oE 'No provider|error 1011|unable to locate|not connected' | head -1 || echo 'device unreachable')"
  log "attempt $attempt: $reason — retrying in ${RETRY_INTERVAL}s (a USB cable makes this instant)"
  sleep "$RETRY_INTERVAL"
done

# ---- launch + surface the on-device diagnostic -------------------------------
if [ "$NO_LAUNCH" = 0 ]; then
  log "Launching + capturing the ConnectionProfileStore diagnostic…"
  timeout 22 xcrun devicectl device process launch --console --terminate-existing \
    --device "$DEVICE_ID" "$BUNDLE_ID" 2>&1 \
    | grep -aE "Launched application|ConnectionProfileStore" | head -6 || true
  pid_line="$(xcrun devicectl device info processes --device "$DEVICE_ID" 2>/dev/null \
    | grep -i 'HoloIroh.app/HoloIroh' | head -1 || true)"
  [ -n "$pid_line" ] && ok "running: $pid_line" || log "(app launched; process list not queryable right now)"
fi

ok "Done."
