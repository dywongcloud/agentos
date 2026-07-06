// app/workflows/agentChatWorkflow.ts
//
// Durable chat turn for a sub-agent's DEDICATED Telegram bot. Inbound updates
// arrive at /api/claw?op=agent_telegram&bot=<botId> (per-bot secret validated
// there), which starts this workflow with the agent's scope. The session id is
// `tgagent:<botId>:<chatId>[:<threadId>]`, so history is isolated per bot+chat
// and the telegram provider resolves the bot's own token for every outbound
// call (replies come FROM the agent's bot, not the main bot).

import type { ModelMessage } from "ai";

import { agentTurn } from "@/app/steps/agentTurn";
import { sendOutbound } from "@/app/steps/sendOutbound";
import { loadHistoryStep, saveHistoryStep } from "@/app/steps/sessionStateSteps";
import { resolveModelName } from "@/app/lib/modelRouting";
import type { SubAgentScope } from "@/app/lib/agents";

function trimHistory(history: ModelMessage[], maxMessages: number): ModelMessage[] {
  const m = Math.max(6, Math.min(200, maxMessages));
  return history.length <= m ? history : history.slice(history.length - m);
}

export async function agentChatWorkflow(args: {
  sessionId: string;
  tenantId: string;
  text: string;
  agent: SubAgentScope;
}) {
  "use workflow";

  const historyRaw = (await loadHistoryStep(args.sessionId)) as ModelMessage[];
  let history = Array.isArray(historyRaw) ? historyRaw : [];

  const max = Number(process.env.HISTORY_MAX_MESSAGES ?? "30");
  history = trimHistory(history, Number.isFinite(max) ? max : 30);

  history.push({ role: "user", content: args.text });

  const chatModel = resolveModelName("chat");

  const result = await agentTurn({
    sessionId: args.sessionId,
    userId: args.tenantId,
    channel: "telegram",
    history,
    showTyping: true,
    modelName: chatModel,
    agent: args.agent,
  });

  history.push({ role: "assistant", content: result.text });
  await saveHistoryStep(args.sessionId, history);

  if (!(result as { delivered?: boolean }).delivered) {
    await sendOutbound({
      channel: "telegram",
      sessionId: args.sessionId,
      text: result.text,
    });
  }
}
