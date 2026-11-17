import { and, eq, ne } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import type { AppContext } from "./context.js";
import { deploymentClient } from "./runtime-client.js";

export class UniverseHandleCacheConflict extends Error {}

export interface UniverseHandleSyncResult { updated: number; skipped: number }

/** Explicit administrative sync. Ordinary reads never call runtime. */
export async function syncUniverseHandles(ctx: AppContext, organizationId?: string): Promise<UniverseHandleSyncResult> {
  let skipped = 0;
  const allRows = await ctx.db.select({ universe: schema.universes, handle: schema.universes.handle })
    .from(schema.universes).innerJoin(schema.organization, eq(schema.organization.id, schema.universes.organizationId));
  const rows = organizationId === undefined ? allRows : allRows.filter(row => row.universe.organizationId === organizationId);
  if (!rows.length) return { updated: 0, skipped: 0 };
  const response = await deploymentClient(ctx.env).call("deployment/universes/list", {});
  const inventory = new Map((response.universes ?? []).map(universe => [universe.universeId, universe]));
  // Other organizations keep their current caches during a scoped sync. Their
  // cached handles still reserve deployment-wide URLs, even if runtime moved on.
  const seen = new Set(organizationId === undefined ? [] : allRows.filter(row => row.universe.organizationId !== organizationId).map(row => row.handle));
  const changes: { universeId: string; handle: string }[] = [];
  const deletions: { id: string; configuration: Record<string, unknown> }[] = [];
  for (const row of rows) {
    const runtime = inventory.get(row.universe.lightspeedUniverseId);
    // Keep the last known cache for a universe the runtime no longer has, so
    // admins can still reach reconciliation and explicitly repair it.
    if (!runtime) skipped += 1;
    if (runtime?.deletion) deletions.push({ id: row.universe.id, configuration: { ...row.universe.runtimeConfiguration, deletion: runtime.deletion, deletionHandle: runtime.handle } });
    const handle = runtime?.deletion ? `_deleting-${runtime.universeId}` : runtime?.handle ?? row.handle;
    if (seen.has(handle)) throw new UniverseHandleCacheConflict("Runtime handles conflict with Platform's cached handles. Resolve conflicting names or request a deployment-wide handle sync.");
    seen.add(handle);
    if (handle !== row.handle) changes.push({ universeId: row.universe.id, handle });
  }
  if (!changes.length && !deletions.length) return { updated: 0, skipped };
  changes.sort((a, b) => a.universeId.localeCompare(b.universeId));
  await ctx.db.transaction(async tx => {
    for (const deletion of deletions) await tx.update(schema.universes).set({ status: "deleting", runtimeConfiguration: deletion.configuration }).where(eq(schema.universes.id, deletion.id));
    // A sequence of runtime renames can swap two cached handles. Clear both
    // inside this transaction before installing the authoritative values.
    for (const change of changes) {
      await tx.update(schema.universes).set({ handle: `handle-cache-${crypto.randomUUID()}` })
        .where(eq(schema.universes.id, change.universeId));
    }
    for (const change of changes) {
      await tx.update(schema.universes).set({ handle: change.handle })
        .where(eq(schema.universes.id, change.universeId));
    }
  });
  return { updated: changes.length, skipped };
}

/** Prevent a known cache collision before mutating runtime. External renames
 * can free a runtime handle while its old Platform cache entry still exists. */
export async function requireAvailableCachedHandle(ctx: AppContext, handle: string, ownUniverseId?: string): Promise<void> {
  const [existing] = await ctx.db.select({ id: schema.universes.id }).from(schema.universes)
    .where(ownUniverseId ? and(eq(schema.universes.handle, handle), ne(schema.universes.id, ownUniverseId)) : eq(schema.universes.handle, handle)).limit(1);
  if (existing) throw new UniverseHandleCacheConflict("This handle is already in Platform's cache. Choose another handle, or ask a Platform administrator to use Sync from runtime if it was renamed externally.");
}
