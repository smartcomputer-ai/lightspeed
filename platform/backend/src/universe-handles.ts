import { and, eq, ne } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import type { AppContext } from "./context.js";
import { deploymentClient } from "./runtime-client.js";

export class UniverseHandleCacheConflict extends Error {}

export interface UniverseHandleSyncResult { updated: number; skipped: number }

/** Explicit administrative sync. Ordinary reads never call runtime. */
export async function syncUniverseHandles(ctx: AppContext): Promise<UniverseHandleSyncResult> {
  let skipped = 0;
  const rows = await ctx.db.select({ universe: schema.universes, handle: schema.organization.slug })
    .from(schema.universes).innerJoin(schema.organization, eq(schema.organization.id, schema.universes.organizationId));
  if (!rows.length) return { updated: 0, skipped: 0 };
  const endpoints = [...new Set(rows.map(row => row.universe.gatewayUrl ?? ctx.env.lightspeedApiUrl))];
  const inventories = new Map(await Promise.all(endpoints.map(async endpoint => {
    const response = await deploymentClient(ctx.env, endpoint).call("deployment/universes/list", {});
    return [endpoint, new Map((response.result.universes ?? []).map(universe => [universe.universeId, universe]))] as const;
  })));
  const seen = new Set<string>();
  const changes: { organizationId: string; handle: string }[] = [];
  for (const row of rows) {
    const runtime = inventories.get(row.universe.gatewayUrl ?? ctx.env.lightspeedApiUrl)?.get(row.universe.lightspeedUniverseId);
    // Keep the last known cache for a universe the runtime no longer has, so
    // admins can still reach reconciliation and explicitly repair it.
    if (!runtime) skipped += 1;
    const handle = runtime?.handle ?? row.handle;
    if (seen.has(handle)) throw new UniverseHandleCacheConflict("Linked runtime universes have conflicting handles; choose distinct handles in runtime before refreshing Platform.");
    seen.add(handle);
    if (handle !== row.handle) changes.push({ organizationId: row.universe.organizationId, handle });
  }
  if (!changes.length) return { updated: 0, skipped };
  changes.sort((a, b) => a.organizationId.localeCompare(b.organizationId));
  await ctx.db.transaction(async tx => {
    // A sequence of runtime renames can swap two cached handles. Clear both
    // inside this transaction before installing the authoritative values.
    for (const change of changes) {
      await tx.update(schema.organization).set({ slug: `handle-cache-${crypto.randomUUID()}` })
        .where(eq(schema.organization.id, change.organizationId));
    }
    for (const change of changes) {
      await tx.update(schema.organization).set({ slug: change.handle })
        .where(eq(schema.organization.id, change.organizationId));
    }
  });
  return { updated: changes.length, skipped };
}

/** Prevent a known cache collision before mutating runtime. External renames
 * can free a runtime handle while its old Platform cache entry still exists. */
export async function requireAvailableCachedHandle(ctx: AppContext, handle: string, ownOrganizationId?: string): Promise<void> {
  const [existing] = await ctx.db.select({ id: schema.organization.id }).from(schema.organization)
    .where(ownOrganizationId ? and(eq(schema.organization.slug, handle), ne(schema.organization.id, ownOrganizationId)) : eq(schema.organization.slug, handle)).limit(1);
  if (existing) throw new UniverseHandleCacheConflict("This handle is already in Platform's cache. Choose another handle, or ask a Platform administrator to use Sync from runtime if it was renamed externally.");
}
