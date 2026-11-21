import type {
  BotCreateParams,
  BotEventAdmitParams,
  BotEventListParams,
  BotEventReadParams,
  BotEventReplayParams,
  BotFilterTestParams,
  BotPutParams,
  BotTriggerPutParams,
} from "@lightspeed-ai/sdk";
import { Hono } from "hono";
import type { UniverseModuleEnv } from "@lightspeed-ai/platform-backend/modules";

// The member gate holds every call to the role its method needs. A trigger as
// it is read or listed omits the webhook ingest path. Bot managers can read
// it through the dedicated method or in the response to writing a trigger.

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

export function botRoutes() {
  const app = new Hono<UniverseModuleEnv>();

  app.get("/", async (c) => {
    const client = c.get("client");
    const response = await client.call("bots/list", {});
    return c.json(response);
  });

  app.post("/", async (c) => {
    const body = await jsonBody(c);
    if (!body) {
      return c.json({ error: "invalid body" }, 400);
    }
    const client = c.get("client");
    const response = await client.call("bots/create", body as unknown as BotCreateParams);
    return c.json(response, 201);
  });

  app.get("/:botId", async (c) => {
    const client = c.get("client");
    const response = await client.call("bots/read", { botId: c.req.param("botId") });
    return c.json(response);
  });

  app.put("/:botId", async (c) => {
    const body = await jsonBody(c);
    const bot = body?.bot;
    if (!body || typeof bot !== "object" || bot === null) {
      return c.json({ error: "invalid body" }, 400);
    }
    const client = c.get("client");
    const response = await client.call("bots/put", {
      bot: { ...bot, botId: c.req.param("botId") },
      expectedRevision: body.expectedRevision,
    } as unknown as BotPutParams);
    return c.json(response);
  });

  app.post("/:botId/close", async (c) => {
    const client = c.get("client");
    const response = await client.call("bots/close", { botId: c.req.param("botId") });
    return c.json(response);
  });

  app.delete("/:botId", async (c) => {
    const client = c.get("client");
    const response = await client.call("bots/delete", { botId: c.req.param("botId") });
    return c.json(response);
  });

  app.get("/:botId/state", async (c) => {
    const client = c.get("client");
    const response = await client.call("bots/state/read", {
      botId: c.req.param("botId"),
    });
    return c.json(response);
  });

  app.post("/:botId/sessions/:sessionId/rotate", async (c) => {
    const client = c.get("client");
    const response = await client.call("bots/sessions/rotate", {
      botId: c.req.param("botId"),
      sessionId: c.req.param("sessionId"),
    });
    return c.json(response);
  });

  app.get("/:botId/triggers", async (c) => {
    const client = c.get("client");
    const response = await client.call("bots/triggers/list", {
      botId: c.req.param("botId"),
    });
    const triggers = response.triggers ?? [];
    return c.json({
      triggers: triggers,
    });
  });

  app.get("/:botId/triggers/:triggerId", async (c) => {
    const client = c.get("client");
    const response = await client.call("bots/triggers/read", {
      botId: c.req.param("botId"),
      triggerId: c.req.param("triggerId"),
    });
    const trigger = response.trigger;
    return c.json({
      trigger: trigger,
    });
  });

  /// The ingest path of a webhook trigger, for members who may manage the bot.
  app.get("/:botId/triggers/:triggerId/secrets", async (c) => {
    const client = c.get("client");
    const response = await client.call("bots/triggers/secrets/read", {
      botId: c.req.param("botId"),
      triggerId: c.req.param("triggerId"),
    });
    return c.json(response);
  });

  app.put("/:botId/triggers/:triggerId", async (c) => {
    const body = await jsonBody(c);
    const trigger = body?.trigger;
    if (!body || typeof trigger !== "object" || trigger === null) {
      return c.json({ error: "invalid body" }, 400);
    }
    const client = c.get("client");
    const response = await client.call("bots/triggers/put", {
      botId: c.req.param("botId"),
      trigger: { ...trigger, triggerId: c.req.param("triggerId") },
      expectedRevision: body.expectedRevision,
    } as unknown as BotTriggerPutParams);
    return c.json(response);
  });

  app.delete("/:botId/triggers/:triggerId", async (c) => {
    const client = c.get("client");
    const response = await client.call("bots/triggers/delete", {
      botId: c.req.param("botId"),
      triggerId: c.req.param("triggerId"),
    });
    return c.json(response);
  });

  app.post("/:botId/events", async (c) => {
    const body = await jsonBody(c);
    const event = body?.event;
    if (!body || typeof event !== "object" || event === null) {
      return c.json({ error: "invalid body" }, 400);
    }
    const client = c.get("client");
    const response = await client.call("bots/events/admit", {
      botId: c.req.param("botId"),
      event,
    } as unknown as BotEventAdmitParams);
    return c.json(response, 202);
  });

  app.post("/:botId/events/replay", async (c) => {
    const body = await jsonBody(c);
    if (!body || typeof body.seq !== "number") {
      return c.json({ error: "invalid body" }, 400);
    }
    const client = c.get("client");
    const response = await client.call("bots/events/replay", {
      botId: c.req.param("botId"),
      seq: body.seq,
    } as unknown as BotEventReplayParams);
    return c.json(response, 202);
  });

  app.get("/:botId/events", async (c) => {
    const limitRaw = c.req.query("limit");
    const limit = limitRaw === undefined ? undefined : Number(limitRaw);
    if (limit !== undefined && !Number.isSafeInteger(limit)) {
      return c.json({ error: "invalid limit" }, 400);
    }
    const client = c.get("client");
    const response = await client.call("bots/events/list", {
      botId: c.req.param("botId"),
      limit,
      cursor: c.req.query("cursor"),
    } as unknown as BotEventListParams);
    return c.json(response);
  });

  app.get("/:botId/events/:seq", async (c) => {
    const seq = Number(c.req.param("seq"));
    if (!Number.isSafeInteger(seq)) {
      return c.json({ error: "invalid seq" }, 400);
    }
    const client = c.get("client");
    const response = await client.call("bots/events/read", {
      botId: c.req.param("botId"),
      seq,
    } as unknown as BotEventReadParams);
    return c.json(response);
  });

  app.post("/:botId/filters/test", async (c) => {
    const body = await jsonBody(c);
    if (!body || typeof body.filter !== "string") {
      return c.json({ error: "invalid body" }, 400);
    }
    const client = c.get("client");
    const response = await client.call("bots/filters/test", {
      botId: c.req.param("botId"),
      filter: body.filter,
      payload: body.payload,
      limit: body.limit,
    } as unknown as BotFilterTestParams);
    return c.json(response);
  });

  return app;
}
