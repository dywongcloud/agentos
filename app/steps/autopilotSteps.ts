// app/steps/autopilotSteps.ts
//
// WDK "use step" wrappers around proactive/autopilot Redis + outbound I/O.
// Same pattern as jobSteps' loadJobMetaStep — a workflow's VM forbids the
// global fetch (and therefore the Upstash REST client, and Telegram sends)
// directly, so any such I/O has to happen inside a step.

import { listProactiveTenants } from "@/app/lib/autopilotProactive";
import {
  getPrimary,
  isAutopilotEnabled,
  getIntervalSeconds,
  type PrimaryTarget,
} from "@/app/lib/autopilotState";
import { sendOutboundRuntime } from "@/app/lib/outbound";
import type { Channel } from "@/app/lib/identity";

export async function listProactiveTenantsStep(): Promise<string[]> {
  "use step";
  return listProactiveTenants();
}

// autopilotWorkflow ("use workflow") called isAutopilotEnabled/getPrimary/
// getIntervalSeconds/sendOutboundRuntime directly in its body — every one of
// them does a real Redis (or Telegram) fetch, which WDK forbids outside a
// step. That crashed the workflow on its very first statement, every single
// invocation, since bootstrap.ts starts it unconditionally on boot.
export async function isAutopilotEnabledStep(): Promise<boolean> {
  "use step";
  return isAutopilotEnabled();
}

export async function getPrimaryStep(): Promise<PrimaryTarget | null> {
  "use step";
  return getPrimary();
}

export async function getIntervalSecondsStep(): Promise<number> {
  "use step";
  return getIntervalSeconds();
}

export async function sendOutboundRuntimeStep(args: {
  channel: Channel;
  sessionId: string;
  text: string;
}): Promise<void> {
  "use step";
  await sendOutboundRuntime(args);
}
