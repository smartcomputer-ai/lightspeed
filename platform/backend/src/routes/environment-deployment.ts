import { Hono } from "hono";
import { z } from "zod";
import type {
  DeploymentEnvironmentProviderPutParams,
  DeploymentProviderBindingPutParams,
} from "@lightspeed-ai/sdk";
import { schema } from "@lightspeed-ai/platform-db";
import type { AppContext, ApiVariables } from "../context.js";
import { isPlatformAdmin } from "../context.js";
import { parseBody } from "../http.js";
import { deploymentClientFor, withGateway } from "./gateway.js";

const providerPutSchema = z.object({
  displayName: z.string().trim().min(1).max(200).optional(),
  controllerEndpoint: z
    .string()
    .trim()
    .max(2048)
    .regex(/^(wss?:\/\/|unix:\/\/\/)[^\s]+$/, "controller endpoint must be a ws://, wss:// or unix:// URL"),
  // Absent derives the data route base from a ws:// or wss:// controller.
  dataRouteBase: z
    .string()
    .trim()
    .max(2048)
    .regex(/^wss?:\/\/[^\s]+$/, "data route base must be a ws:// or wss:// URL")
    .optional(),
  // Write-only and optional: omitted keeps the stored credential.
  credential: z.string().min(1).max(4096).optional(),
  clearCredential: z.boolean().optional(),
}).refine(
  (value) => !(value.credential && value.clearCredential),
  "credential and clearCredential are mutually exclusive",
);
const bindingPutSchema = z.object({
  enabled: z.boolean(),
});

/// Deployment environment administration. Universe owners consume
/// enabled bindings elsewhere; only platform admins may mutate physical
/// provider registrations or universe admission bindings here.
export function environmentDeploymentRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  app.use("*", async (c, next) => {
    if (!isPlatformAdmin(c.get("session"))) {
      return c.json({ error: "platform admin required" }, 403);
    }
    await next();
  });

  app.get("/environment-providers", (c) => withGateway(c, async () => {
    const response = await deploymentClientFor(ctx).call("deployment/environment-providers/list", {});
    return c.json(response.result.providers);
  }));

  /// Deployment-wide binding inventory: every platform universe on the
  /// default deployment with its provider bindings, so admins can see which
  /// universes may provision from which provider. Universes on another
  /// gateway are reported without bindings.
  app.get("/environment-provider-bindings", (c) => withGateway(c, async () => {
    const rows = await ctx.db.select().from(schema.universes);
    const universes = await Promise.all(rows.map(async (row) => {
      const base = {
        universeId: row.id,
        lightspeedUniverseId: row.lightspeedUniverseId,
        name: row.name,
        status: row.status,
      };
      if (row.gatewayUrl) {
        return { ...base, bindings: [], error: "universe lives on another deployment" };
      }
      try {
        const response = await deploymentClientFor(ctx).call(
          "deployment/environment-provider-bindings/list",
          { universeId: row.lightspeedUniverseId },
        );
        return { ...base, bindings: response.result.bindings ?? [], error: null };
      } catch (error) {
        return {
          ...base,
          bindings: [],
          error: error instanceof Error ? error.message : String(error),
        };
      }
    }));
    return c.json(universes);
  }));

  app.put("/environment-providers/:providerId", async (c) => {
    const body = await parseBody(c, providerPutSchema);
    if (!body.ok) return body.response;
    return withGateway(c, async () => {
      const params: DeploymentEnvironmentProviderPutParams = {
        providerId: c.req.param("providerId"),
        ...body.data,
      };
      const response = await deploymentClientFor(ctx).call(
        "deployment/environment-providers/put",
        params,
      );
      return c.json(response.result.provider);
    });
  });

  app.delete("/environment-providers/:providerId", (c) => withGateway(c, async () => {
    const response = await deploymentClientFor(ctx).call(
      "deployment/environment-providers/delete",
      { providerId: c.req.param("providerId") },
    );
    return c.json(response.result.provider);
  }));

  app.put("/universes/:universeId/environment-provider-bindings/:providerId", async (c) => {
    const body = await parseBody(c, bindingPutSchema);
    if (!body.ok) return body.response;
    return withGateway(c, async () => {
      const params: DeploymentProviderBindingPutParams = {
        universeId: c.req.param("universeId"),
        providerId: c.req.param("providerId"),
        enabled: body.data.enabled,
      };
      const response = await deploymentClientFor(ctx).call(
        "deployment/environment-providers/bindings/put",
        params,
      );
      return c.json(response.result.binding);
    });
  });

  app.delete("/universes/:universeId/environment-provider-bindings/:providerId", (c) =>
    withGateway(c, async () => {
      const response = await deploymentClientFor(ctx).call(
        "deployment/environment-providers/bindings/delete",
        {
          universeId: c.req.param("universeId"),
          providerId: c.req.param("providerId"),
        },
      );
      return c.json(response.result.binding);
    }));

  return app;
}
