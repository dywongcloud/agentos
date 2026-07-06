// app/lib/registerAutomation.ts
//
// Materializes a compiled trigger/action spec into a live Automation rule:
// computes the first schedule fire, subscribes the Composio trigger, mints the
// webhook secret, and persists the rule. Shared by the /automate + /team
// command handlers (app/api/claw/route.ts) and the chat agent's
// create_workforce tool (app/tools/workforceTools.ts).

import type { Channel } from "@/app/lib/identity";
import type { CompiledAutomation } from "@/app/steps/compileAutomationStep";
import {
  putAutomation,
  nextScheduleAt,
  type Automation,
  type AutomationTrigger,
} from "@/app/lib/automations";
import { subscribeTrigger } from "@/app/lib/composioTriggers";
import {
  getCustomTriggerType,
  subscribeCustomTrigger,
} from "@/app/lib/customTriggers";

export async function registerAutomation(args: {
  tenantId: string;
  channel: Channel;
  sessionId: string;
  spec: string;
  compiled: CompiledAutomation;
  baseUrl: string;
  // Subscription config for composio/custom triggers that need it
  // (e.g. { board_id } for the monday polling triggers).
  triggerConfig?: Record<string, unknown>;
}): Promise<{ rule: Automation; note: string }> {
  const { compiled } = args;
  let trigger: AutomationTrigger;
  let note = "";

  switch (compiled.trigger.kind) {
    case "schedule": {
      const t = {
        kind: "schedule" as const,
        ...(compiled.trigger.cron ? { cron: compiled.trigger.cron } : {}),
        ...(compiled.trigger.everyMs ? { everyMs: compiled.trigger.everyMs } : {}),
        ...(compiled.trigger.tz ? { tz: compiled.trigger.tz } : {}),
        nextAt: 0,
      };
      t.nextAt = nextScheduleAt(t, Date.now());
      trigger = t;
      note = `next fire: ${new Date(t.nextAt).toISOString()}`;
      break;
    }
    case "composio": {
      trigger = {
        kind: "composio",
        triggerType: compiled.trigger.triggerType,
        ...(compiled.trigger.filter ? { filter: compiled.trigger.filter } : {}),
      };
      // Custom (polling) trigger types — e.g. the monday.com set — live in our
      // local registry, not Composio. Subscribe the local poller instead; its
      // events fan out to matching automations just like real webhooks.
      const custom = await getCustomTriggerType(compiled.trigger.triggerType);
      if (custom) {
        const sub = await subscribeCustomTrigger({
          tenantId: args.tenantId,
          slug: custom.slug,
          config: args.triggerConfig,
        });
        if (sub.ok) {
          trigger.triggerId = sub.subId;
          note = `watching ${custom.slug} via local polling (${sub.subId})`;
        } else {
          note = `⚠️ couldn't subscribe ${custom.slug}: ${sub.error}`;
        }
        break;
      }
      // Best-effort: subscribe the underlying Composio trigger so events flow.
      const sub = await subscribeTrigger({
        tenantId: args.tenantId,
        slug: compiled.trigger.triggerType,
        ...(args.triggerConfig ? { triggerConfig: args.triggerConfig } : {}),
      });
      if (sub.ok) {
        trigger.triggerId = sub.triggerId;
        note = `subscribed ${compiled.trigger.triggerType} (${sub.triggerId})`;
      } else {
        note = `⚠️ couldn't subscribe ${compiled.trigger.triggerType}: ${sub.error}. Connect the app, then /automate resume the rule.`;
      }
      break;
    }
    case "webhook": {
      const secret = globalThis.crypto.randomUUID().replace(/-/g, "");
      trigger = { kind: "webhook", secret };
      // URL is finalized after we know the rule id (below).
      note = secret;
      break;
    }
    case "chat": {
      trigger = {
        kind: "chat",
        pattern: compiled.trigger.pattern,
        flags: compiled.trigger.flags ?? "i",
      };
      note = `fires on messages matching /${trigger.pattern}/${trigger.flags}`;
      break;
    }
  }

  const rule = await putAutomation({
    tenantId: args.tenantId,
    channel: args.channel,
    sessionId: args.sessionId,
    name: compiled.name,
    spec: args.spec,
    trigger,
    action: compiled.action,
    enabled: true,
  });

  if (rule.trigger.kind === "webhook") {
    note = `${args.baseUrl}/api/claw?op=auto_webhook&id=${rule.id}&secret=${rule.trigger.secret}`;
  }

  return { rule, note };
}
