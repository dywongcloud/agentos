// app/steps/autopilotSteps.ts
//
// WDK "use step" wrapper around the proactive tenants list. Same pattern as
// jobSteps' loadJobMetaStep — the daemon workflow's VM forbids the Upstash
// SDK directly, so any Redis access has to happen inside a step.

import { listProactiveTenants } from "@/app/lib/autopilotProactive";

export async function listProactiveTenantsStep(): Promise<string[]> {
  "use step";
  return listProactiveTenants();
}
