// app/lib/teams.ts
//
// "Teams" — a shared tenant namespace. Multiple users (each with their own
// per-user identity, e.g. telegram:123) belong to a Team; when they talk to the
// bot inside the team's bound Telegram GROUP chat, their messages resolve to the
// team's tenantId (`team:<id>`) instead of their individual one. Because almost
// every user-scoped subsystem (Composio connections, automations, triggers,
// jobs/deep-jobs, /code projects, VFS, memory) keys off `tenantId`, sharing
// falls out automatically once the namespace is remapped.
//
// CONCURRENCY: the roster mutates from CONCURRENT group messages (several people
// talking at once). It is therefore NOT stored in the record blob (a
// GET→mutate→SET there loses writes under races). Instead the mutable roster is
// decomposed into atomic Redis structures — each mutation is a single race-free
// command (separation logic: disjoint state):
//   team:members:{id}  HASH  tenantId -> TeamMember JSON   (HSETNX = atomic add)
//   team:left:{id}     SET   tenantIds who explicitly left (SADD/SREM/SISMEMBER)
// The blob holds only rarely-mutated scalars (name/owner/token/binding).
//
// Redis layout (all under `team:`, no TTL — standing records):
//   team:rec:{id}                JSON  TeamRecord (scalars only)
//   team:members:{id}            HASH  roster
//   team:left:{id}               SET   opted-out members
//   team:by_group:{chatId}       STR   teamId   (bound Telegram group → team)
//   team:by_token:{token}        STR   teamId   (invite deep-link → team)
//   team:by_user:{memberTenant}  SET   teamIds  (teams a user belongs to)

import { getStore } from "@/app/lib/store";
import type { Channel } from "@/app/lib/identity";

// --- types --------------------------------------------------------------

export type TeamMember = {
  tenantId: string; // the member's per-user identity, e.g. "telegram:123"
  senderId: string; // raw sender id (telegram user id)
  username?: string;
  role: "owner" | "member";
  joinedAt: number;
};

// Persisted scalar record (roster lives in separate atomic structures).
type TeamRecord = {
  id: string;
  name: string;
  channel: Channel;
  ownerTenantId: string;
  tgGroupChatId?: string;
  inviteToken: string;
  createdAt: number;
};

export type Team = TeamRecord & {
  members: TeamMember[];
  leftTenantIds: string[];
};

// --- id helpers ---------------------------------------------------------

function newTeamId(): string {
  return "tm_" + globalThis.crypto.randomUUID().replace(/-/g, "").slice(0, 12);
}
function newInviteToken(): string {
  // URL/deep-link safe, no colons (the tenantId format splits on ':').
  return globalThis.crypto.randomUUID().replace(/-/g, "").slice(0, 16);
}

// --- tenantId namespace -------------------------------------------------

export function teamTenantId(teamId: string): string {
  return `team:${teamId}`;
}
export function isTeamTenant(tenantId: string): boolean {
  return typeof tenantId === "string" && tenantId.startsWith("team:");
}
export function teamIdFromTenant(tenantId: string): string | null {
  return isTeamTenant(tenantId) ? tenantId.slice("team:".length) : null;
}

// Where to deliver an event/notification addressed to a team tenant: the team's
// bound group chat. Reads only the scalar record (no roster fetch).
export async function teamGroupSession(
  tenantId: string
): Promise<{ channel: Channel; sessionId: string } | null> {
  const teamId = teamIdFromTenant(tenantId);
  if (!teamId) return null;
  const rec = await getRecord(teamId);
  if (!rec?.tgGroupChatId) return null;
  return { channel: rec.channel, sessionId: `${rec.channel}:${rec.tgGroupChatId}` };
}

// --- keys ---------------------------------------------------------------

const recKey = (id: string) => `team:rec:${id}`;
const membersKey = (id: string) => `team:members:${id}`;
const leftKey = (id: string) => `team:left:${id}`;
const byGroupKey = (chatId: string) => `team:by_group:${chatId}`;
const byTokenKey = (token: string) => `team:by_token:${token}`;
const byUserKey = (memberTenantId: string) => `team:by_user:${memberTenantId}`;

// --- group→team binding cache (hot path) --------------------------------
//
// Every group message resolves its team from the group chat id. Bindings change
// only on create/rebind/delete (rare), so an in-process TTL cache eliminates a
// Redis read per message on repeat traffic. Negative results are cached too (so
// non-team groups — the common case — don't hit Redis each message). Bounded
// staleness via TTL; create/bind/delete invalidate the local entry immediately.

const BINDING_TTL_MS = 30_000;
const bindingCache = new Map<string, { id: string | null; exp: number }>();

function invalidateBinding(chatId: string): void {
  bindingCache.delete(String(chatId));
}

// The teamId bound to a group chat id (cached), or null. Placeholder
// (`pending_…`) claims from an in-flight create are treated as "no team yet".
export async function teamIdForGroupChat(chatId: string): Promise<string | null> {
  const key = String(chatId);
  const now = Date.now();
  const hit = bindingCache.get(key);
  if (hit && hit.exp > now) return hit.id;
  const raw = await getStore().get<string>(byGroupKey(key));
  const id = raw && raw.startsWith("tm_") ? raw : null;
  // Bound memory (¬∃leak): drop the whole cache past a ceiling rather than grow
  // unboundedly across an instance's lifetime — entries are cheap to rebuild.
  if (bindingCache.size > 5000) bindingCache.clear();
  bindingCache.set(key, { id, exp: now + BINDING_TTL_MS });
  return id;
}

// --- record + roster reads ----------------------------------------------

async function getRecord(id: string): Promise<TeamRecord | null> {
  return getStore().get<TeamRecord>(recKey(id));
}

export async function getTeam(id: string): Promise<Team | null> {
  const store = getStore();
  const rec = await getRecord(id);
  if (!rec) return null;
  const [membersMap, left] = await Promise.all([
    store.hgetall<TeamMember>(membersKey(id)),
    store.smembers(leftKey(id)),
  ]);
  const members = Object.values(membersMap).sort((a, b) => a.joinedAt - b.joinedAt);
  return { ...rec, members, leftTenantIds: left };
}

export async function getTeamByGroupChat(chatId: string): Promise<Team | null> {
  const id = await teamIdForGroupChat(chatId);
  return id ? getTeam(id) : null;
}

export async function getTeamByInviteToken(token: string): Promise<Team | null> {
  const id = await getStore().get<string>(byTokenKey(token));
  return id && id.startsWith("tm_") ? getTeam(id) : null;
}

export async function listTeamsForUser(memberTenantId: string): Promise<Team[]> {
  const ids = await getStore().smembers(byUserKey(memberTenantId));
  const out = (await Promise.all(ids.map((id) => getTeam(id)))).filter(
    (t): t is Team => t !== null
  );
  out.sort((a, b) => b.createdAt - a.createdAt);
  return out;
}

// --- lifecycle ----------------------------------------------------------

export async function createTeam(args: {
  name: string;
  channel: Channel;
  ownerTenantId: string;
  ownerSenderId: string;
  ownerUsername?: string;
  tgGroupChatId?: string;
}): Promise<Team> {
  const store = getStore();
  // Atomic claim of the group binding (NX) so two simultaneous
  // `/workspace create` calls in the same group can't mint two teams.
  if (args.tgGroupChatId) {
    const placeholder = "pending_" + globalThis.crypto.randomUUID().slice(0, 8);
    const fresh = await store.set(byGroupKey(args.tgGroupChatId), placeholder, {
      nx: true,
      exSeconds: 30,
    });
    if (!fresh) {
      for (let i = 0; i < 3; i++) {
        const existing = await getTeamByGroupChat(args.tgGroupChatId);
        if (existing) return existing;
        await new Promise((r) => setTimeout(r, 300));
      }
    }
  }

  const rec: TeamRecord = {
    id: newTeamId(),
    name: args.name.trim() || "Team",
    channel: args.channel,
    ownerTenantId: args.ownerTenantId,
    tgGroupChatId: args.tgGroupChatId,
    inviteToken: newInviteToken(),
    createdAt: Date.now(),
  };
  const owner: TeamMember = {
    tenantId: args.ownerTenantId,
    senderId: args.ownerSenderId,
    username: args.ownerUsername,
    role: "owner",
    joinedAt: Date.now(),
  };
  await store.set(recKey(rec.id), rec);
  await store.set(byTokenKey(rec.inviteToken), rec.id);
  await store.hsetnx(membersKey(rec.id), owner.tenantId, owner);
  await store.sadd(byUserKey(owner.tenantId), rec.id);
  if (rec.tgGroupChatId) {
    await store.set(byGroupKey(rec.tgGroupChatId), rec.id); // replace placeholder
    invalidateBinding(rec.tgGroupChatId);
  }
  return { ...rec, members: [owner], leftTenantIds: [] };
}

// Bind (or rebind) a Telegram group chat to a team; clears any previous binding.
export async function bindGroupChat(teamId: string, chatId: string): Promise<Team | null> {
  const store = getStore();
  const rec = await getRecord(teamId);
  if (!rec) return null;
  if (rec.tgGroupChatId && rec.tgGroupChatId !== String(chatId)) {
    await store.del(byGroupKey(rec.tgGroupChatId));
    invalidateBinding(rec.tgGroupChatId);
  }
  rec.tgGroupChatId = String(chatId);
  await store.set(recKey(teamId), rec);
  await store.set(byGroupKey(rec.tgGroupChatId), teamId);
  invalidateBinding(rec.tgGroupChatId);
  return getTeam(teamId);
}

// --- roster mutations (atomic, race-free) -------------------------------

// Auto-enroll from a group message. Race-free: SISMEMBER gate + atomic HSETNX
// (concurrent messages from the same new member → exactly one insert). Skips
// anyone who explicitly left. No full-record read/write.
export async function recordMemberAuto(
  teamId: string,
  member: Omit<TeamMember, "role" | "joinedAt">
): Promise<void> {
  const store = getStore();
  if (await store.sismember(leftKey(teamId), member.tenantId)) return;
  const created = await store.hsetnx(membersKey(teamId), member.tenantId, {
    ...member,
    role: "member",
    joinedAt: Date.now(),
  } satisfies TeamMember);
  if (created) await store.sadd(byUserKey(member.tenantId), teamId);
}

// Explicit join (invite link / token): clears the left mark, then atomic add.
export async function addMember(
  teamId: string,
  member: Omit<TeamMember, "role" | "joinedAt"> & { role?: TeamMember["role"] }
): Promise<Team | null> {
  const store = getStore();
  const rec = await getRecord(teamId);
  if (!rec) return null;
  await store.srem(leftKey(teamId), member.tenantId);
  const created = await store.hsetnx(membersKey(teamId), member.tenantId, {
    tenantId: member.tenantId,
    senderId: member.senderId,
    username: member.username,
    role: member.role ?? "member",
    joinedAt: Date.now(),
  } satisfies TeamMember);
  if (created) await store.sadd(byUserKey(member.tenantId), teamId);
  return getTeam(teamId);
}

export async function removeMember(teamId: string, tenantId: string): Promise<Team | null> {
  const store = getStore();
  const rec = await getRecord(teamId);
  if (!rec) return null;
  if (rec.ownerTenantId === tenantId) return getTeam(teamId); // owner: delete instead
  await store.sadd(leftKey(teamId), tenantId); // mark left BEFORE removing (no re-enroll window)
  await store.hdel(membersKey(teamId), tenantId);
  await store.srem(byUserKey(tenantId), teamId);
  return getTeam(teamId);
}

export async function hasUserLeft(teamId: string, tenantId: string): Promise<boolean> {
  return getStore().sismember(leftKey(teamId), tenantId);
}

export function hasLeft(team: Team, tenantId: string): boolean {
  return team.leftTenantIds.includes(tenantId);
}
export function isMember(team: Team, tenantId: string): boolean {
  return team.members.some((m) => m.tenantId === tenantId);
}

export async function renameTeam(teamId: string, name: string): Promise<Team | null> {
  const rec = await getRecord(teamId);
  if (!rec) return null;
  rec.name = name.trim() || rec.name;
  await getStore().set(recKey(teamId), rec);
  return getTeam(teamId);
}

export async function deleteTeam(id: string): Promise<boolean> {
  const store = getStore();
  const rec = await getRecord(id);
  if (!rec) return false;
  const membersMap = await store.hgetall<TeamMember>(membersKey(id));
  for (const m of Object.values(membersMap)) await store.srem(byUserKey(m.tenantId), id);
  if (rec.tgGroupChatId) {
    await store.del(byGroupKey(rec.tgGroupChatId));
    invalidateBinding(rec.tgGroupChatId);
  }
  await store.del(byTokenKey(rec.inviteToken));
  await store.del(membersKey(id));
  await store.del(leftKey(id));
  await store.del(recKey(id));
  return true;
}
