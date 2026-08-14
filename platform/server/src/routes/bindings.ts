import { Hono, type Context } from "hono";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed/platform-db";
import {
  bindingCreateSchema,
  bindingUpdateSchema,
  mintPairingCode,
} from "@lightspeed/platform-shared";
import type { AppContext, ApiVariables } from "../context.js";
import { parseBody } from "../http.js";
import { universeForSession } from "./universes.js";

const { bindings, channelAccounts } = schema;

/// Binding routes are universe-scoped: /universes/:id/bindings for
/// list/create, /bindings/:id for update/delete (access derived from the
/// binding's universe).
export function bindingRoutes(ctx: AppContext) {
  const byUniverse = new Hono<{ Variables: ApiVariables }>();

  byUniverse.get("/:id/bindings", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), false);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const rows = await ctx.db
      .select({
        binding: bindings,
        channelAccount: {
          id: channelAccounts.id,
          provider: channelAccounts.provider,
          accountId: channelAccounts.accountId,
          displayName: channelAccounts.displayName,
          enabled: channelAccounts.enabled,
        },
      })
      .from(bindings)
      .innerJoin(channelAccounts, eq(channelAccounts.id, bindings.channelAccountId))
      .where(eq(bindings.universeId, access.universe.id));
    return c.json(rows.map(({ binding, channelAccount }) => ({ ...binding, channelAccount })));
  });

  byUniverse.post("/:id/bindings", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, bindingCreateSchema);
    if (!body.ok) {
      return body.response;
    }
    const { pairingCode, ...rest } = body.data;
    const [created] = await ctx.db
      .insert(bindings)
      .values({
        ...rest,
        universeId: access.universe.id,
        // Omitted → mint one; explicit null → open binding (no pairing).
        pairingCode: pairingCode === undefined ? mintPairingCode() : pairingCode,
      })
      .returning();
    return c.json(created, 201);
  });

  const byId = new Hono<{ Variables: ApiVariables }>();

  const bindingAccess = async (
    c: Context<{ Variables: ApiVariables }>,
    write: boolean,
  ) => {
    const [row] = await ctx.db
      .select()
      .from(bindings)
      .where(eq(bindings.id, c.req.param("id") ?? ""))
      .limit(1);
    if (!row) {
      return null;
    }
    const access = await universeForSession(ctx, c, row.universeId, write);
    return access ? row : null;
  };

  byId.get("/:id", async (c) => {
    const row = await bindingAccess(c, false);
    if (!row) {
      return c.json({ error: "not found" }, 404);
    }
    return c.json(row);
  });

  byId.patch("/:id", async (c) => {
    const row = await bindingAccess(c, true);
    if (!row) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, bindingUpdateSchema);
    if (!body.ok) {
      return body.response;
    }
    const [updated] = await ctx.db
      .update(bindings)
      .set(body.data)
      .where(eq(bindings.id, row.id))
      .returning();
    return c.json(updated);
  });

  byId.post("/:id/rotate-pairing", async (c) => {
    const row = await bindingAccess(c, true);
    if (!row) {
      return c.json({ error: "not found" }, 404);
    }
    const [updated] = await ctx.db
      .update(bindings)
      .set({ pairingCode: mintPairingCode() })
      .where(eq(bindings.id, row.id))
      .returning();
    return c.json(updated);
  });

  byId.delete("/:id", async (c) => {
    const row = await bindingAccess(c, true);
    if (!row) {
      return c.json({ error: "not found" }, 404);
    }
    await ctx.db.delete(bindings).where(eq(bindings.id, row.id));
    return c.json({ ok: true });
  });

  return { byUniverse, byId };
}
