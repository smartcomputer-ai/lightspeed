import { Hono } from "hono";
import { z } from "zod";
import { eq } from "drizzle-orm";
import type { MethodGroup } from "@lightspeed-ai/agent-client";
import { schema } from "@lightspeed/platform-db";
import type { AppContext, ApiVariables } from "../context.js";
import { isPlatformAdmin } from "../context.js";
import { parseBody } from "../http.js";
import { deploymentClientFor, withGateway } from "./gateway.js";

const createSchema = z.object({
  displayName: z.string().trim().min(1).max(120),
  /// The deployment, or a Platform universe by its Platform id.
  scope: z.discriminatedUnion("kind", [
    z.object({ kind: z.literal("deployment") }),
    z.object({ kind: z.literal("universe"), universeId: z.string().uuid() }),
  ]),
  /// Absent grants every group the scope allows; core validates the names.
  groups: z.array(z.string().min(1).max(100)).max(32).optional(),
  assertActor: z.boolean().default(false),
});

/// Every core key of the deployment, for platform admins: what each reaches
/// and may call, and whether it may assert actors. Keys are immutable; a
/// change is revoke and mint. The secret exists only in the create response.
export function apiKeyAdminRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  app.use("*", async (c, next) => {
    if (!isPlatformAdmin(c.get("session"))) {
      return c.json({ error: "platform admin required" }, 403);
    }
    await next();
  });

  app.get("/api-keys", (c) => withGateway(c, async () => {
    const response = await deploymentClientFor(ctx).call("deployment/api-keys/list", {});
    return c.json(response.result.apiKeys ?? []);
  }));

  app.post("/api-keys", async (c) => {
    const body = await parseBody(c, createSchema);
    if (!body.ok) {
      return body.response;
    }
    const input = body.data;
    let scope: { kind: "deployment" } | { kind: "universe"; universeId: string } = { kind: "deployment" };
    if (input.scope.kind === "universe") {
      const [universe] = await ctx.db
        .select({ lightspeedUniverseId: schema.universes.lightspeedUniverseId })
        .from(schema.universes)
        .where(eq(schema.universes.id, input.scope.universeId))
        .limit(1);
      if (!universe) {
        return c.json({ error: "universe not found" }, 404);
      }
      scope = { kind: "universe", universeId: universe.lightspeedUniverseId };
    }
    return withGateway(c, async () => {
      const response = await deploymentClientFor(ctx).call("deployment/api-keys/create", {
        scope,
        displayName: input.displayName,
        ...(input.groups ? { groups: input.groups as MethodGroup[] } : {}),
        assertActor: input.assertActor,
      });
      return c.json(response.result, 201);
    });
  });

  app.delete("/api-keys/:keyPrefix", (c) => withGateway(c, async () => {
    const response = await deploymentClientFor(ctx).call("deployment/api-keys/revoke", {
      keyPrefix: c.req.param("keyPrefix"),
    });
    return c.json(response.result.apiKey);
  }));

  return app;
}
