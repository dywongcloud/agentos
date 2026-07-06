// app/lib/dashboards.ts
//
// CRUD + ordering + cache for dashboard widgets, over getStore(). Keys:
//   dash:w:{id}            JSON WidgetSpec
//   dash:by_tenant:{t}     SET of widget ids owned by a tenant
//   dash:order:{t}         JSON string[] — widget id render order
//   dash:cache:{id}        JSON WidgetData, written with an exSeconds TTL
//
// Widgets are pull, not push: the executor computes data on read and caches it
// with a TTL, so there's no per-tick daemon work here.

import { getStore } from "@/app/lib/store";
import type { WidgetSpec, WidgetData } from "@/app/lib/widgetSpec";

const wKey = (id: string) => `dash:w:${id}`;
const byTenantKey = (t: string) => `dash:by_tenant:${t}`;
const orderKey = (t: string) => `dash:order:${t}`;
const cacheKey = (id: string) => `dash:cache:${id}`;

export const DEFAULT_WIDGET_TTL_SECONDS = 300;

export function newWidgetId(): string {
  return `w_${Date.now().toString(36)}_${Math.random().toString(36).slice(2, 8)}`;
}

export async function getWidget(id: string): Promise<WidgetSpec | null> {
  return getStore().get<WidgetSpec>(wKey(id));
}

// Return a tenant's widgets in saved render order. Ids in the order list that
// no longer resolve are skipped; ids present but missing from the order list
// (e.g. a race) are appended.
export async function listWidgets(tenantId: string): Promise<WidgetSpec[]> {
  const store = getStore();
  const ids = await store.smembers(byTenantKey(tenantId));
  if (ids.length === 0) return [];

  const order = (await store.get<string[]>(orderKey(tenantId))) ?? [];
  const ordered = [
    ...order.filter((id) => ids.includes(id)),
    ...ids.filter((id) => !order.includes(id)),
  ];

  const specs = await Promise.all(ordered.map((id) => store.get<WidgetSpec>(wKey(id))));
  return specs.filter((s): s is WidgetSpec => s != null);
}

export async function putWidget(spec: WidgetSpec): Promise<WidgetSpec> {
  const store = getStore();
  await store.set(wKey(spec.id), spec);
  await store.sadd(byTenantKey(spec.tenantId), spec.id);
  // Newly created widgets land at the end of the order list.
  const order = (await store.get<string[]>(orderKey(spec.tenantId))) ?? [];
  if (!order.includes(spec.id)) {
    await store.set(orderKey(spec.tenantId), [...order, spec.id]);
  }
  return spec;
}

export async function deleteWidget(tenantId: string, id: string): Promise<void> {
  const store = getStore();
  await store.del(wKey(id));
  await store.del(cacheKey(id));
  await store.srem(byTenantKey(tenantId), id);
  const order = (await store.get<string[]>(orderKey(tenantId))) ?? [];
  await store.set(orderKey(tenantId), order.filter((x) => x !== id));
}

// Persist an explicit order. Only ids the tenant actually owns are kept.
export async function reorderWidgets(tenantId: string, ids: string[]): Promise<void> {
  const store = getStore();
  const owned = await store.smembers(byTenantKey(tenantId));
  const next = ids.filter((id) => owned.includes(id));
  await store.set(orderKey(tenantId), next);
}

export async function getCachedData(id: string): Promise<WidgetData | null> {
  return getStore().get<WidgetData>(cacheKey(id));
}

export async function setCachedData(
  id: string,
  data: WidgetData,
  ttlSeconds = DEFAULT_WIDGET_TTL_SECONDS
): Promise<void> {
  await getStore().set(cacheKey(id), data, { exSeconds: ttlSeconds });
}
