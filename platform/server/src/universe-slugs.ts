import { and, eq, ne } from "drizzle-orm";
import { schema } from "@lightspeed/platform-db";
import type { AppContext } from "./context.js";
import { deploymentClient } from "./runtime-client.js";

export class UniverseSlugCacheConflict extends Error {}

export interface UniverseSlugSyncResult { updated: number; skipped: number }

/** Explicit administrative sync. Ordinary reads never call runtime. */
export async function syncUniverseSlugs(ctx: AppContext): Promise<UniverseSlugSyncResult> {
  let skipped = 0;
  const rows = await ctx.db.select({ universe: schema.universes, slug: schema.organization.slug })
    .from(schema.universes).innerJoin(schema.organization, eq(schema.organization.id, schema.universes.organizationId));
  if (!rows.length) return { updated: 0, skipped: 0 };
  const endpoints = [...new Set(rows.map(row => row.universe.gatewayUrl ?? ctx.env.lightspeedApiUrl))];
  const inventories = new Map(await Promise.all(endpoints.map(async endpoint => {
    const response = await deploymentClient(ctx.env, endpoint).call("deployment/universes/list", {});
    return [endpoint, new Map((response.result.universes ?? []).map(universe => [universe.universeId, universe]))] as const;
  })));
  const seen = new Set<string>();
  const changes: { organizationId: string; slug: string }[] = [];
  for (const row of rows) {
    const runtime = inventories.get(row.universe.gatewayUrl ?? ctx.env.lightspeedApiUrl)?.get(row.universe.lightspeedUniverseId);
    // Keep the last known cache for missing/unnamed legacy rows so admins
    // can still reach reconciliation and explicitly repair them.
    if (!runtime?.slug) skipped += 1;
    const slug = runtime?.slug ?? row.slug;
    if (seen.has(slug)) throw new UniverseSlugCacheConflict("Linked runtime universes have conflicting slugs; choose distinct slugs in runtime before refreshing Platform.");
    seen.add(slug);
    if (slug !== row.slug) changes.push({ organizationId: row.universe.organizationId, slug });
  }
  if (!changes.length) return { updated: 0, skipped };
  changes.sort((a, b) => a.organizationId.localeCompare(b.organizationId));
  await ctx.db.transaction(async tx => {
    // A sequence of runtime renames can swap two cached slugs. Clear both
    // inside this transaction before installing the authoritative values.
    for (const change of changes) {
      await tx.update(schema.organization).set({ slug: `slug-cache-${crypto.randomUUID()}` })
        .where(eq(schema.organization.id, change.organizationId));
    }
    for (const change of changes) {
      await tx.update(schema.organization).set({ slug: change.slug })
        .where(eq(schema.organization.id, change.organizationId));
    }
  });
  return { updated: changes.length, skipped };
}

/** Prevent a known cache collision before mutating runtime. External renames
 * can free a runtime slug while its old Platform cache entry still exists. */
export async function requireAvailableCachedSlug(ctx: AppContext, slug: string, ownOrganizationId?: string): Promise<void> {
  const [existing] = await ctx.db.select({ id: schema.organization.id }).from(schema.organization)
    .where(ownOrganizationId ? and(eq(schema.organization.slug, slug), ne(schema.organization.id, ownOrganizationId)) : eq(schema.organization.slug, slug)).limit(1);
  if (existing) throw new UniverseSlugCacheConflict("This slug is already in Platform's cache. Choose another slug, or ask a Platform administrator to use Sync from runtime if it was renamed externally.");
}
