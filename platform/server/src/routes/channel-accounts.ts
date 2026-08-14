import { Hono } from "hono";
import { eq } from "drizzle-orm";
import { schema } from "@lightspeed/platform-db";
import {
  channelAccountCreateSchema,
  channelAccountUpdateSchema,
} from "@lightspeed/platform-shared";
import type { AppContext, ApiVariables } from "../context.js";
import { isPlatformAdmin } from "../context.js";
import { parseBody } from "../http.js";

const { channelAccounts } = schema;
const publicFields = {
  id: channelAccounts.id,
  provider: channelAccounts.provider,
  accountId: channelAccounts.accountId,
  displayName: channelAccounts.displayName,
  settings: channelAccounts.settings,
  enabled: channelAccounts.enabled,
  createdAt: channelAccounts.createdAt,
  updatedAt: channelAccounts.updatedAt,
};

export function channelAccountRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  app.get("/", async (c) => {
    const rows = await ctx.db
      .select(publicFields)
      .from(channelAccounts);
    return c.json(rows);
  });

  app.post("/", async (c) => {
    if (!isPlatformAdmin(c.get("session"))) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const body = await parseBody(c, channelAccountCreateSchema);
    if (!body.ok) return body.response;
    const [created] = await ctx.db
      .insert(channelAccounts)
      .values(body.data)
      .returning(publicFields);
    return c.json(created, 201);
  });

  app.patch("/:id", async (c) => {
    if (!isPlatformAdmin(c.get("session"))) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const body = await parseBody(c, channelAccountUpdateSchema);
    if (!body.ok) return body.response;
    const [updated] = await ctx.db
      .update(channelAccounts)
      .set(body.data)
      .where(eq(channelAccounts.id, c.req.param("id")))
      .returning(publicFields);
    return updated === undefined
      ? c.json({ error: "not found" }, 404)
      : c.json(updated);
  });

  app.delete("/:id", async (c) => {
    if (!isPlatformAdmin(c.get("session"))) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const [deleted] = await ctx.db
      .delete(channelAccounts)
      .where(eq(channelAccounts.id, c.req.param("id")))
      .returning({ id: channelAccounts.id });
    return deleted === undefined
      ? c.json({ error: "not found" }, 404)
      : c.json({ ok: true });
  });

  return app;
}
