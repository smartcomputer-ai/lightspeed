import { Hono } from "hono";
import { z } from "zod";
import type { ApiVariables, AppContext } from "../context.js";
import { parseBody } from "../http.js";
import { engineClientFor, withGateway } from "./gateway.js";
import { universeForSession } from "./universes.js";
import { grantSchema, resourceSchema } from "./access-schemas.js";

const policyRead = z.object({ resource: resourceSchema }).strict();
const policyPut = policyRead.extend({
  visibility: z.enum(["universe", "restricted"]),
  grants: z.array(grantSchema),
  owner: z.string().uuid().optional(),
  expectedRevision: z.number().int().nonnegative(),
});

/** All calls retain the signed-in principal; core owns every permission decision. */
export function accessRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.get("/:id/access/subjects", (c) =>
    withGateway(c, async () => {
      const access = await universeForSession(ctx, c, c.req.param("id"));
      if (!access) return c.json({ error: "not found" }, 404);
      const query = c.req.query("q") ?? "";
      if (new TextEncoder().encode(query).length > 200)
        return c.json({ error: "Search is too long" }, 400);
      return c.json(
        (
          await engineClientFor(ctx, access.universe).call("access/subjects", {
            query,
          })
        ).result,
      );
    }),
  );
  app.post("/:id/access/policy/read", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) return c.json({ error: "not found" }, 404);
    const body = await parseBody(c, policyRead);
    if (!body.ok) return body.response;
    return withGateway(c, async () =>
      c.json(
        (
          await engineClientFor(ctx, access.universe).call(
            "access/policy/read",
            body.data,
          )
        ).result,
      ),
    );
  });
  app.put("/:id/access/policy", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) return c.json({ error: "not found" }, 404);
    const body = await parseBody(c, policyPut);
    if (!body.ok) return body.response;
    return withGateway(c, async () =>
      c.json(
        (
          await engineClientFor(ctx, access.universe).call(
            "access/policy/put",
            body.data,
          )
        ).result,
      ),
    );
  });
  app.get("/:id/access/execution", (c) =>
    withGateway(c, async () => {
      const access = await universeForSession(ctx, c, c.req.param("id"));
      if (!access) return c.json({ error: "not found" }, 404);
      return c.json(
        (
          await engineClientFor(ctx, access.universe).call(
            "access/execution/read",
            {},
          )
        ).result,
      );
    }),
  );
  app.put("/:id/access/execution", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) return c.json({ error: "not found" }, 404);
    const body = await parseBody(
      c,
      z.object({ personalExecutionEnabled: z.boolean() }).strict(),
    );
    if (!body.ok) return body.response;
    return withGateway(c, async () =>
      c.json(
        (
          await engineClientFor(ctx, access.universe).call(
            "access/execution/update",
            body.data,
          )
        ).result,
      ),
    );
  });
  return app;
}
