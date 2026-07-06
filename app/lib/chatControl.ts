// app/lib/chatControl.ts
//
// Shared "is this chat halted?" gate used by /stop and /start. Kept in one place
// so both the inbound handler (which sets it) and long-running work (agent-turn
// typing loops, the job workflow) can read the SAME flag to halt promptly.
//
// /stop sets the flag (and cancels active work); /start clears it. The flag is a
// simple per-(channel, sessionId) Redis key so a halt scopes to one chat/group.

import { getStore } from "@/app/lib/store";

export function chatStopKey(channel: string, sessionId: string): string {
  return `chat:stopped:${channel}:${sessionId}`;
}

export async function isChatStopped(channel: string, sessionId: string): Promise<boolean> {
  return (await getStore().get(chatStopKey(channel, sessionId))) === "1";
}

// Mark a chat halted. Long TTL: it stays quiet until an explicit /start.
export async function setChatStopped(channel: string, sessionId: string): Promise<void> {
  await getStore().set(chatStopKey(channel, sessionId), "1", {
    exSeconds: 60 * 60 * 24 * 365,
  });
}

// Clear the halt. Writes "0" with a short TTL (rather than deleting) so any
// in-flight loop that reads it mid-transition sees a definitive "not stopped".
export async function clearChatStopped(channel: string, sessionId: string): Promise<void> {
  await getStore().set(chatStopKey(channel, sessionId), "0", { exSeconds: 5 });
}
