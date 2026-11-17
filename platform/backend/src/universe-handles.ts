import { and, eq, ne } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import type { AppContext } from "./context.js";

export class UniverseHandleCacheConflict extends Error {}

/** Prevent a known cache collision before mutating runtime. External renames
 * can free a runtime handle while its old Platform cache entry still exists. */
export async function requireAvailableCachedHandle(ctx: AppContext, handle: string, ownUniverseId?: string): Promise<void> {
  const [existing] = await ctx.db.select({ id: schema.universes.id }).from(schema.universes)
    .where(ownUniverseId ? and(eq(schema.universes.handle, handle), ne(schema.universes.id, ownUniverseId)) : eq(schema.universes.handle, handle)).limit(1);
  if (existing) throw new UniverseHandleCacheConflict("This handle is already in Platform's cache. Choose another handle. If it was renamed outside Platform, try again in a minute, once Platform has picked up the change.");
}
