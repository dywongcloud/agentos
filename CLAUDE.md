@AGENTS.md

# Deploying to production

Use `scripts/deploy-prod.sh` instead of raw `vercel --prod --yes`. The raw
command does **not** auto-alias `agentos-claw.vercel.app` — this is a
recurring gotcha across multiple past sessions.

Root cause: `agentos-claw.vercel.app` is a real, verified project domain
(auto-provisioned at project creation), but it was never added to the
project's server-side `targets.production.automaticAliases` list — only
the two default `*-blockoffset.vercel.app` domains are in that list. There
is no CLI flag or `vercel.json` setting that controls this; it can only be
fixed permanently via the Vercel Dashboard (Project → Settings → Domains →
remove & re-add `agentos-claw.vercel.app`, or Settings → Environments →
Production → Branch Tracking). Until that's done, `vercel --prod --yes`
alone will leave the live domain pointed at a stale deployment.

`scripts/deploy-prod.sh` runs the deploy and then automatically runs
`vercel alias set <new-deployment-url> agentos-claw.vercel.app` as its
final step, so a single script call always leaves the domain correctly
pointed with no manual follow-up to forget.
