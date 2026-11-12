import { Hono } from "hono";
import { z } from "zod";
import { eq } from "drizzle-orm";
import type { AccessScope, MethodGroup } from "@lightspeed-ai/sdk";
import { schema } from "@lightspeed-ai/platform-db";
import { chosenKeyIdSchema } from "@lightspeed-ai/platform-shared";
import type { AppContext, ApiVariables } from "../context.js";
import { isPlatformAdmin } from "../context.js";
import { auditIdentity } from "../identity-audit.js";
import { parseBody } from "../http.js";
import { createApiKey, deploymentClientFor, withGateway } from "./gateway.js";

// Shape only: runtime universe ids need not be RFC 4122 conformant.
const runtimeUniverseId = z.string().regex(/^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/);

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
  /// Absent has the runtime allocate one. Create only: an id never changes.
  keyId: chosenKeyIdSchema.optional(),
});

/// Every core key of the deployment, for platform admins: what each reaches
/// and may call, and whether it may assert actors. A key is addressed by its
/// key id within its scope. Secrets are returned only on create or rotation.
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
      const created = await createApiKey(c, deploymentClientFor(ctx), {
        scope,
        displayName: input.displayName,
        ...(input.groups ? { groups: input.groups as MethodGroup[] } : {}),
        assertActor: input.assertActor,
        ...(input.keyId ? { keyId: input.keyId } : {}),
      });
      if (created instanceof Response) return created;
      await auditIdentity(ctx.db, { actorId: c.get("session").user.id, action: "key.create", targetId: created.apiKey.keyId, universeId: input.scope.kind === "universe" ? input.scope.universeId : null, details: { keyPrefix: created.apiKey.keyPrefix } });
      return c.json(created, 201);
    });
  });

  /// A key id names a key within its scope, so these name the scope too:
  /// the runtime universe in `universeId`, or the deployment when absent.
  const keyScope = (universeId: string | undefined): AccessScope | null => {
    if (universeId === undefined) return { kind: "deployment" };
    return runtimeUniverseId.safeParse(universeId).success ? { kind: "universe", universeId } : null;
  };

  app.post("/api-keys/:keyId/rotate", (c) => withGateway(c, async () => {
    const scope = keyScope(c.req.query("universeId"));
    if (!scope) return c.json({ error: "invalid universeId" }, 400);
    const keyId = c.req.param("keyId");
    const response = await deploymentClientFor(ctx).call("deployment/api-keys/rotate", { scope, keyId });
    await auditIdentity(ctx.db, { actorId: c.get("session").user.id, action: "key.rotate", targetId: keyId, details: { newKeyPrefix: response.result.apiKey.keyPrefix } });
    return c.json(response.result);
  }));

  app.delete("/api-keys/:keyId", (c) => withGateway(c, async () => {
    const scope = keyScope(c.req.query("universeId"));
    if (!scope) return c.json({ error: "invalid universeId" }, 400);
    const keyId = c.req.param("keyId");
    const response = await deploymentClientFor(ctx).call("deployment/api-keys/revoke", { scope, keyId });
    await auditIdentity(ctx.db, { actorId: c.get("session").user.id, action: "key.revoke", targetId: keyId });
    return c.json(response.result.apiKey);
  }));

  return app;
}
