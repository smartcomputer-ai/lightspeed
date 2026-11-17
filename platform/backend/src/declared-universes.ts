import { eq, sql } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import { LightspeedApiError } from "@lightspeed-ai/sdk";
import type { AppContext } from "./context.js";
import { deploymentClient } from "./runtime-client.js";

/** Sync runtime metadata only for explicitly organization-linked resources. Runtime
 * documents cannot create Platform organizations, memberships or Owner grants. */
export async function syncDeclaredUniverses(ctx: AppContext): Promise<{ adopted: number; updated: number }> {
  if (!ctx.env.lightspeedApiUrl || !ctx.env.lightspeedApiToken) return { adopted: 0, updated: 0 };
  return ctx.db.transaction(async tx => {
    const locked = await tx.execute(sql`SELECT pg_try_advisory_xact_lock(731845920114) AS locked`);
    if (!locked.rows[0]?.locked) return { adopted: 0, updated: 0 };
    const client = deploymentClient(ctx.env);
    const inventory = (await client.call("deployment/universes/list", {}, { signal: AbortSignal.timeout(10000) })).universes ?? [];
    const rows = await tx.select().from(schema.universes);
    const current = new Map(inventory.map(universe => [universe.universeId, universe]));
    const updates: Array<{ row: typeof rows[number]; runtime: typeof inventory[number] }> = [];
    for (const row of rows) {
      const runtime = current.get(row.lightspeedUniverseId);
      if (runtime) { updates.push({ row, runtime }); continue; }
      if (row.status === "provisioning") continue;
      try { await client.call("deployment/universes/read", { universe: row.lightspeedUniverseId }, { signal: AbortSignal.timeout(10000) }); }
      catch (error) {
        if (!(error instanceof LightspeedApiError)) throw error;
        if (error.kind === "gone") await tx.delete(schema.universes).where(eq(schema.universes.id, row.id));
        else if (error.kind !== "notFound") throw error;
      }
    }
    for (const { row, runtime } of updates) if (runtime.handle !== row.handle) await tx.update(schema.universes).set({ handle: `sync-${crypto.randomUUID()}` }).where(eq(schema.universes.id, row.id));
    for (const { row, runtime } of updates) {
      await tx.update(schema.universes).set({
        name: runtime.displayName ?? row.name, handle: runtime.deletion ? `_deleting-${runtime.universeId}` : runtime.handle,
        runtimeConfiguration: { displayName: runtime.displayName, metadata: runtime.metadata, protected: runtime.protected, expiresAtMs: runtime.expiresAtMs, ...(runtime.deletion ? { deletion: runtime.deletion, deletionHandle: runtime.handle } : {}) },
        ...(runtime.deletion ? { status: "deleting" as const } : {}),
      }).where(eq(schema.universes.id, row.id));
    }
    return { adopted: 0, updated: updates.length };
  });
}

export function startDeclaredUniverseSync(ctx: AppContext): () => void {
  let stopped = false; let timer: ReturnType<typeof setTimeout> | undefined;
  const run = async () => {
    try { await syncDeclaredUniverses(ctx); }
    catch (error) { console.error("[universes] Runtime metadata sync failed; retrying in 30 seconds", error instanceof Error ? error.message : "unknown error"); }
    if (!stopped) { timer = setTimeout(() => { void run(); }, 30000); timer.unref(); }
  };
  void run(); return () => { stopped = true; if (timer) clearTimeout(timer); };
}
