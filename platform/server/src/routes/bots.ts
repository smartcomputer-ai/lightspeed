import type {
  BotCreateParams,
  BotEventAdmitParams,
  BotEventListParams,
  BotEventReadParams,
  BotEventReplayParams,
  BotFilterTestParams,
  BotPutParams,
  BotTriggerPutParams,
} from "@lightspeed-ai/agent-client";
import { Hono } from "hono";
import type { AppContext, ApiVariables } from "../context.js";
import { engineClientFor, withGateway } from "./gateway.js";
import { universeForSession } from "./universes.js";

// The runtime enforces current user permissions and redacts trigger secrets.

async function jsonBody(c: {
  req: { json: () => Promise<unknown> };
}): Promise<Record<string, unknown> | null> {
  try {
    const body = await c.req.json();
    return typeof body === "object" && body !== null
      ? (body as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

export function botRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  app.get("/:id/bots", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/list", {});
      return c.json(response.result);
    });
  });

  app.post("/:id/bots", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await jsonBody(c);
    if (!body) {
      return c.json({ error: "invalid body" }, 400);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/create", body as unknown as BotCreateParams);
      return c.json(response.result, 201);
    });
  });

  app.get("/:id/bots/:botId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/read", { botId: c.req.param("botId") });
      return c.json(response.result);
    });
  });

  app.put("/:id/bots/:botId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await jsonBody(c);
    const bot = body?.bot;
    if (!body || typeof bot !== "object" || bot === null) {
      return c.json({ error: "invalid body" }, 400);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/put", {
        bot: { ...bot, botId: c.req.param("botId") },
        expectedRevision: body.expectedRevision,
      } as unknown as BotPutParams);
      return c.json(response.result);
    });
  });

  app.post("/:id/bots/:botId/close", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/close", { botId: c.req.param("botId") });
      return c.json(response.result);
    });
  });

  app.delete("/:id/bots/:botId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/delete", { botId: c.req.param("botId") });
      return c.json(response.result);
    });
  });

  app.get("/:id/bots/:botId/state", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/state/read", {
        botId: c.req.param("botId"),
      });
      return c.json(response.result);
    });
  });

  app.post("/:id/bots/:botId/sessions/:sessionId/rotate", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/sessions/rotate", {
        botId: c.req.param("botId"),
        sessionId: c.req.param("sessionId"),
      });
      return c.json(response.result);
    });
  });

  app.get("/:id/bots/:botId/triggers", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/triggers/list", {
        botId: c.req.param("botId"),
      });
      const triggers = response.result.triggers ?? [];
      return c.json({
        triggers: triggers,
      });
    });
  });

  app.get("/:id/bots/:botId/triggers/:triggerId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/triggers/read", {
        botId: c.req.param("botId"),
        triggerId: c.req.param("triggerId"),
      });
      const trigger = response.result.trigger;
      return c.json({
        trigger: trigger,
      });
    });
  });

  app.put("/:id/bots/:botId/triggers/:triggerId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await jsonBody(c);
    const trigger = body?.trigger;
    if (!body || typeof trigger !== "object" || trigger === null) {
      return c.json({ error: "invalid body" }, 400);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/triggers/put", {
        botId: c.req.param("botId"),
        trigger: { ...trigger, triggerId: c.req.param("triggerId") },
        expectedRevision: body.expectedRevision,
      } as unknown as BotTriggerPutParams);
      return c.json(response.result);
    });
  });

  app.delete("/:id/bots/:botId/triggers/:triggerId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/triggers/delete", {
        botId: c.req.param("botId"),
        triggerId: c.req.param("triggerId"),
      });
      return c.json(response.result);
    });
  });

  app.post("/:id/bots/:botId/events", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await jsonBody(c);
    const event = body?.event;
    if (!body || typeof event !== "object" || event === null) {
      return c.json({ error: "invalid body" }, 400);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/events/admit", {
        botId: c.req.param("botId"),
        event,
      } as unknown as BotEventAdmitParams);
      return c.json(response.result, 202);
    });
  });

  app.post("/:id/bots/:botId/events/replay", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await jsonBody(c);
    if (!body || typeof body.seq !== "number") {
      return c.json({ error: "invalid body" }, 400);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/events/replay", {
        botId: c.req.param("botId"),
        seq: body.seq,
      } as unknown as BotEventReplayParams);
      return c.json(response.result, 202);
    });
  });

  app.get("/:id/bots/:botId/events", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const limitRaw = c.req.query("limit");
    const limit = limitRaw === undefined ? undefined : Number(limitRaw);
    if (limit !== undefined && !Number.isSafeInteger(limit)) {
      return c.json({ error: "invalid limit" }, 400);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/events/list", {
        botId: c.req.param("botId"),
        limit,
        cursor: c.req.query("cursor"),
      } as unknown as BotEventListParams);
      return c.json(response.result);
    });
  });

  app.get("/:id/bots/:botId/events/:seq", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const seq = Number(c.req.param("seq"));
    if (!Number.isSafeInteger(seq)) {
      return c.json({ error: "invalid seq" }, 400);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/events/read", {
        botId: c.req.param("botId"),
        seq,
      } as unknown as BotEventReadParams);
      return c.json(response.result);
    });
  });

  app.post("/:id/bots/:botId/filters/test", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await jsonBody(c);
    if (!body || typeof body.filter !== "string") {
      return c.json({ error: "invalid body" }, 400);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access);
      const response = await client.call("bots/filters/test", {
        botId: c.req.param("botId"),
        filter: body.filter,
        payload: body.payload,
        limit: body.limit,
      } as unknown as BotFilterTestParams);
      return c.json(response.result);
    });
  });

  return app;
}
