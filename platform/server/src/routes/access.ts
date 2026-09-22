import { Hono } from "hono";
import { z } from "zod";
import type { ApiVariables, AppContext } from "../context.js";
import { parseBody } from "../http.js";
import { engineClientFor, withGateway } from "./gateway.js";
import { universeForSession } from "./universes.js";
import {
  accessInputSchema,
  executionInputSchema,
  grantSchema,
  resourceSchema,
} from "./access-schemas.js";

const policyRead = z.object({ resource: resourceSchema }).strict();
const policyPut = policyRead.extend({
  visibility: z.enum(["universe", "restricted"]),
  grants: z.array(grantSchema),
  owner: z.string().uuid().optional(),
  expectedRevision: z.number().int().nonnegative(),
});
const collectionCreate = z
  .object({
    displayName: z.string().trim().min(1).max(200),
    access: accessInputSchema.optional(),
    execution: executionInputSchema.optional(),
  })
  .strict();

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
  app.get("/:id/collections", (c) =>
    withGateway(c, async () => {
      const access = await universeForSession(ctx, c, c.req.param("id"));
      if (!access) return c.json({ error: "not found" }, 404);
      return c.json(
        (
          await engineClientFor(ctx, access.universe).call(
            "collection/list",
            {},
          )
        ).result,
      );
    }),
  );
  app.post("/:id/collections", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) return c.json({ error: "not found" }, 404);
    const body = await parseBody(c, collectionCreate);
    if (!body.ok) return body.response;
    return withGateway(c, async () =>
      c.json(
        (
          await engineClientFor(ctx, access.universe).call(
            "collection/create",
            body.data,
          )
        ).result,
        201,
      ),
    );
  });
  app.get("/:id/collections/:collectionId", (c) =>
    withGateway(c, async () => {
      const access = await universeForSession(ctx, c, c.req.param("id"));
      if (!access) return c.json({ error: "not found" }, 404);
      return c.json(
        (
          await engineClientFor(ctx, access.universe).call("collection/read", {
            collectionId: c.req.param("collectionId"),
          })
        ).result,
      );
    }),
  );
  app.put("/:id/collections/:collectionId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) return c.json({ error: "not found" }, 404);
    const body = await parseBody(
      c,
      z
        .object({
          displayName: z.string().trim().min(1).max(200),
          expectedRevision: z.number().int().nonnegative(),
        })
        .strict(),
    );
    if (!body.ok) return body.response;
    return withGateway(c, async () =>
      c.json(
        (
          await engineClientFor(ctx, access.universe).call(
            "collection/update",
            { ...body.data, collectionId: c.req.param("collectionId") },
          )
        ).result,
      ),
    );
  });
  app.delete("/:id/collections/:collectionId", (c) =>
    withGateway(c, async () => {
      const access = await universeForSession(ctx, c, c.req.param("id"));
      if (!access) return c.json({ error: "not found" }, 404);
      return c.json(
        (
          await engineClientFor(ctx, access.universe).call(
            "collection/delete",
            { collectionId: c.req.param("collectionId") },
          )
        ).result,
      );
    }),
  );
  return app;
}
