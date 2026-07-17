#!/usr/bin/env bash
#
# deploy-prod.sh
#
# Wrapper around `vercel --prod --yes` that guarantees the custom domain
# agentos-claw.vercel.app is re-pointed at the new production deployment.
#
# WHY THIS EXISTS:
# The Vercel project `blockoffset/agentos-claw` was provisioned with
# agentos-claw.vercel.app as a verified project domain, but it was never
# added to the project's `targets.production.automaticAliases` list
# server-side. That list (not `vercel.json`, not any CLI flag) is what
# controls which domains get automatically re-aliased on every
# `vercel --prod` deploy. Only the two default `*-blockoffset.vercel.app`
# domains are in that list, so raw `vercel --prod --yes` silently leaves
# agentos-claw.vercel.app pointed at whatever deployment it was last
# manually aliased to. There is no CLI/vercel.json setting to fix the
# underlying `automaticAliases` list -- that requires the Vercel Dashboard
# (Project -> Settings -> Domains -> remove & re-add the domain, or
# Settings -> Environments -> Production -> Branch Tracking). Until that's
# done there, this script is the durable workaround: it always finishes by
# explicitly aliasing agentos-claw.vercel.app to whatever was just deployed,
# so there's no manual follow-up step to forget.
#
# Usage:
#   scripts/deploy-prod.sh [extra args passed through to `vercel deploy`]
#
set -euo pipefail

DOMAIN="agentos-claw.vercel.app"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

LOG_FILE="$(mktemp -t deploy-prod-log)"
trap 'rm -f "$LOG_FILE"' EXIT

echo "==> Running: vercel --prod --yes (cwd: $PROJECT_DIR)"

# Vercel CLI writes ONLY the deployment URL to stdout on success (all
# progress/build logs go to stderr); we also request JSON as a
# belt-and-suspenders in case stdout instead contains a JSON payload on
# this CLI version. Both stdout and stderr are tee'd to a log file so we
# can fall back to regex-scanning the full output if neither of the
# structured parses works.
set +e
DEPLOY_STDOUT="$(cd "$PROJECT_DIR" && vercel deploy --prod --yes -F json "$@" 2> >(tee "$LOG_FILE" >&2))"
DEPLOY_EXIT=$?
set -e

if [ $DEPLOY_EXIT -ne 0 ]; then
  echo "ERROR: vercel deploy failed (exit $DEPLOY_EXIT). Not aliasing $DOMAIN." >&2
  exit $DEPLOY_EXIT
fi

echo "$DEPLOY_STDOUT" >> "$LOG_FILE"

DEPLOY_URL=""

# 1) Try to parse stdout as JSON (`.url` or `.alias[0]`, with or without scheme).
if command -v jq >/dev/null 2>&1; then
  PARSED="$(printf '%s' "$DEPLOY_STDOUT" | jq -r '(.url // .alias[0] // empty)' 2>/dev/null || true)"
  if [ -n "$PARSED" ] && [ "$PARSED" != "null" ]; then
    DEPLOY_URL="$PARSED"
  fi
fi

# 2) Fall back to treating stdout itself as the bare deployment URL (the
#    documented behavior of `vercel deploy` when not producing JSON).
if [ -z "$DEPLOY_URL" ]; then
  CANDIDATE="$(printf '%s' "$DEPLOY_STDOUT" | tr -d '[:space:]')"
  if printf '%s' "$CANDIDATE" | grep -qE '^(https://)?[a-zA-Z0-9.-]+\.vercel\.app$'; then
    DEPLOY_URL="$CANDIDATE"
  fi
fi

# 3) Last resort: scan the full combined log for any *.vercel.app deployment
#    URL (matches the `<project>-<hash>-<team>.vercel.app` deployment
#    hostname pattern, i.e. excludes the two known static alias domains).
if [ -z "$DEPLOY_URL" ]; then
  DEPLOY_URL="$(grep -oE 'https?://[a-zA-Z0-9.-]+\.vercel\.app' "$LOG_FILE" \
    | grep -v -- "-blockoffset\.vercel\.app$" \
    | tail -1 || true)"
fi

if [ -z "$DEPLOY_URL" ]; then
  echo "ERROR: Could not determine the deployment URL from 'vercel deploy' output." >&2
  echo "       Raw output has been preserved below for manual aliasing:" >&2
  cat "$LOG_FILE" >&2
  exit 1
fi

# Strip protocol -- `vercel alias set` wants a bare hostname/id.
DEPLOY_URL="${DEPLOY_URL#https://}"
DEPLOY_URL="${DEPLOY_URL#http://}"

echo "==> Deployment URL: https://$DEPLOY_URL"
echo "==> Aliasing $DOMAIN -> $DEPLOY_URL"

vercel alias set "$DEPLOY_URL" "$DOMAIN"

echo "==> Done. https://$DOMAIN now points at https://$DEPLOY_URL"
