import { Hono } from "hono";
import { z } from "zod";
import type {
  OperatorEnvironmentProviderPutParams,
  OperatorProviderBindingPutParams,
} from "@lightspeed/agent-client";
import type { AppContext, ApiVariables } from "../context.js";
import { isPlatformAdmin } from "../context.js";
import { parseBody } from "../http.js";
import { operatorClientFor, withGateway } from "./gateway.js";

const metadataSchema = z.record(z.string(), z.string()).optional();
const providerPutSchema = z.object({
  displayName: z.string().trim().min(1).max(200).optional(),
  metadata: metadataSchema,
  controllerConnection: z.object({
    endpoint: z.string().url(),
    transport: z.object({ type: z.enum(["webSocket", "http"]) }),
  }),
});
const bindingPutSchema = z.object({
  providerId: z.string().trim().min(1).max(200),
  status: z.enum(["enabled", "disabled"]),
  expectedRevision: z.number().int().min(1).optional(),
  metadata: metadataSchema,
});

/// Deployment/operator environment administration. Universe owners consume
/// enabled bindings elsewhere; only platform admins may mutate physical
/// provider registrations or universe admission bindings here.
export function environmentOperatorRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  app.use("*", async (c, next) => {
    if (!isPlatformAdmin(c.get("session"))) {
      return c.json({ error: "platform admin required" }, 403);
    }
    await next();
  });

  app.get("/environment-providers", (c) => withGateway(c, async () => {
    const response = await operatorClientFor(ctx).call("operator/environment-providers/list", {});
    return c.json(response.result.providers);
  }));

  app.put("/environment-providers/:providerId", async (c) => {
    const body = await parseBody(c, providerPutSchema);
    if (!body.ok) return body.response;
    return withGateway(c, async () => {
      const response = await operatorClientFor(ctx).call(
        "operator/environment-providers/put",
        {
          providerId: c.req.param("providerId"),
          ...body.data,
        } as OperatorEnvironmentProviderPutParams,
      );
      return c.json(response.result.provider);
    });
  });

  app.delete("/environment-providers/:providerId", (c) => withGateway(c, async () => {
    const response = await operatorClientFor(ctx).call(
      "operator/environment-providers/delete",
      { providerId: c.req.param("providerId") },
    );
    return c.json(response.result.provider);
  }));

  app.put("/universes/:universeId/environment-provider-bindings/:bindingId", async (c) => {
    const body = await parseBody(c, bindingPutSchema);
    if (!body.ok) return body.response;
    return withGateway(c, async () => {
      const response = await operatorClientFor(ctx).call(
        "operator/environment-providers/bindings/put",
        {
          universeId: c.req.param("universeId"),
          bindingId: c.req.param("bindingId"),
          ...body.data,
        } as OperatorProviderBindingPutParams,
      );
      return c.json(response.result.binding);
    });
  });

  app.delete("/universes/:universeId/environment-provider-bindings/:bindingId", (c) =>
    withGateway(c, async () => {
      const response = await operatorClientFor(ctx).call(
        "operator/environment-providers/bindings/delete",
        {
          universeId: c.req.param("universeId"),
          bindingId: c.req.param("bindingId"),
        },
      );
      return c.json(response.result.binding);
    }));

  return app;
}
