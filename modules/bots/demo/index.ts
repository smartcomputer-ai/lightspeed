import { Hono, type Context, type MiddlewareHandler } from "hono";
import type { UniverseAction } from "@lightspeed-ai/platform-shared";
import type { BotEventDocument, BotFilterTestResult, BotListItem } from "@lightspeed-ai/sdk";
import {
  asRecord, badRequest, conflict, demoAccess, demoUniverseActions, intQuery, isRecord, notFound,
  readBody, universeFor, type DemoStore,
} from "@lightspeed-ai/platform-web/demo";
import {
  BotConfigError, admitBotEvent, botErrorResponse, botSessionsOf, botViewOf, closeBot, createBot,
  listItemOf, putBot, putTrigger, requestBotSessionRotation, triggerSecretsOf, triggerViewOf,
} from "@lightspeed-ai/platform-web/demo/bots";

const MAX_HISTORY_LIMIT = 100;
const COLLATOR = new Intl.Collator("en", { sensitivity: "base", numeric: true });

function optionalString(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function botFor(store: DemoStore, c: Context) {
  const universe = universeFor(store, c);
  const record = universe?.bots.get(c.req.param("botId") ?? "");
  return universe && record ? { universe, record } : null;
}

export default function botRoutes(store: DemoStore): Hono {
  const app = new Hono();
  app.onError((error, c) => botErrorResponse(c, error));

  function requireAction(action: UniverseAction): MiddlewareHandler {
    return async (c, next) => {
      const universe = universeFor(store, c);
      if (!universe) return notFound(c);
      if (!demoUniverseActions(store, universe).has(action)) return c.json({ error: `universe permission required: ${action}` }, 403);
      await next();
    };
  }

  app.get("/", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const bots: BotListItem[] = [...universe.bots.values()]
      .sort((left, right) => {
        const byLabel = COLLATOR.compare(
          left.bot.displayName ?? left.bot.botId,
          right.bot.displayName ?? right.bot.botId,
        );
        return byLabel !== 0 ? byLabel : COLLATOR.compare(left.bot.botId, right.bot.botId);
      })
      .map((record) => listItemOf(universe, record));
    return c.json({ bots });
  });

  app.post("/", requireAction("createBot"), async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody(c);
    const bot = asRecord(body.bot);
    if (!bot) return badRequest(c, "invalid body");
    const created = createBot(store, universe, bot, body.triggers);
    if (created instanceof BotConfigError) return botErrorResponse(c, created);
    const triggers = [...created.record.triggers.values()];
    const triggerSecrets = Object.fromEntries(
      triggers
        .map((trigger) => [trigger.triggerId, triggerSecretsOf(trigger)] as const)
        .filter(([, secrets]) => Object.keys(secrets).length > 0),
    );
    return c.json(
      {
        bot: botViewOf(created.record),
        triggers: triggers.map(triggerViewOf),
        ...(Object.keys(triggerSecrets).length > 0 ? { triggerSecrets } : {}),
      },
      201,
    );
  });

  app.get("/:botId", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    // As the server: the bot and its audience.
    return c.json({ bot: botViewOf(found.record), access: demoAccess() });
  });

  /// Whole-document replace with an expected revision; an unknown id
  /// creates the bot, like the core's `bots/put`.
  app.put("/:botId", requireAction("manageBot"), async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const botId = c.req.param("botId") ?? "";
    const body = await readBody(c);
    const input = asRecord(body.bot);
    if (!input) return badRequest(c, "invalid body");
    return c.json({ bot: putBot(store, universe, botId, input, body.expectedRevision) });
  });

  app.post("/:botId/close", requireAction("manageBot"), (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    closeBot(found.universe, found.record);
    return c.json({ bot: botViewOf(found.record) });
  });

  app.delete("/:botId", requireAction("manageBot"), (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const { universe, record } = found;
    closeBot(universe, record);
    const deletedSessions: string[] = [];
    universe.bots.delete(record.bot.botId);
    return c.json({ bot: botViewOf(record), deletedSessions });
  });

  app.get("/:botId/state", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    return c.json({ state: { controller: found.record.state, sessions: botSessionsOf(found.universe, found.record) } });
  });

  app.post("/:botId/sessions/:sessionId/rotate", requireAction("manageBot"), (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const { universe, record } = found;
    const sessionId = c.req.param("sessionId") ?? "";
    requestBotSessionRotation(store, universe, record, sessionId);
    return c.json({ accepted: true });
  });

  app.get("/:botId/triggers", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const triggers = [...found.record.triggers.values()]
      .sort((left, right) => left.triggerId.localeCompare(right.triggerId))
      .map(triggerViewOf);
    return c.json({ triggers });
  });

  app.get("/:botId/triggers/:triggerId", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const trigger = found.record.triggers.get(c.req.param("triggerId") ?? "");
    if (!trigger) return notFound(c);
    return c.json({ trigger: triggerViewOf(trigger) });
  });

  /// The ingest path of one trigger, like the core's
  /// `bots/triggers/secrets/read`; an empty object for a trigger with neither.
  app.get("/:botId/triggers/:triggerId/secrets", requireAction("manageBot"), (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const trigger = found.record.triggers.get(c.req.param("triggerId") ?? "");
    if (!trigger) return notFound(c);
    return c.json(triggerSecretsOf(trigger));
  });

  /// Whole-document replace with an expected revision; an unknown id
  /// creates the trigger, like the core's `bots/triggers/put`.
  app.put("/:botId/triggers/:triggerId", requireAction("manageBot"), async (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const { universe, record } = found;
    if (record.bot.closedAtMs != null) return conflict(c, "bot is closed");
    const triggerId = c.req.param("triggerId") ?? "";
    const body = await readBody(c);
    const input = asRecord(body.trigger);
    if (!input) return badRequest(c, "invalid body");
    try {
      const trigger = putTrigger(universe, record, triggerId, input, body.expectedRevision);
      const secrets = triggerSecretsOf(trigger);
      return c.json({
        trigger: triggerViewOf(trigger),
        ...(Object.keys(secrets).length > 0 ? { secrets } : {}),
      });
    } catch (error) {
      return botErrorResponse(c, error);
    }
  });

  app.delete("/:botId/triggers/:triggerId", requireAction("manageBot"), (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const triggerId = c.req.param("triggerId") ?? "";
    const trigger = found.record.triggers.get(triggerId);
    if (!trigger) return notFound(c);
    found.record.triggers.delete(triggerId);
    return c.json({ trigger: triggerViewOf(trigger) });
  });

  /// Operator admit: `{ event: BotEventInput }`, answered 202.
  app.post("/:botId/events", requireAction("invokeBot"), async (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const { universe, record } = found;
    if (record.bot.closedAtMs != null) return c.json({ error: "bot is closed" }, 410);
    if (record.bot.enabled === false) return conflict(c, "bot is disabled");
    const body = await readBody(c);
    const input = asRecord(body.event);
    if (!input) return badRequest(c, "invalid body");
    const kind = optionalString(input.kind);
    const summary = optionalString(input.summary);
    if (!kind || !summary) return badRequest(c, "validation failed: kind and summary are required");
    const links = Array.isArray(input.links)
      ? input.links.filter((link): link is string => typeof link === "string")
      : undefined;
    const headers = isRecord(input.headers)
      ? Object.fromEntries(
          Object.entries(input.headers).filter((entry): entry is [string, string] => typeof entry[1] === "string"),
        )
      : undefined;
    const { event, duplicate } = admitBotEvent(store, universe, record, {
      kind,
      source: "manual",
      summary,
      payload: input.data,
      ...(typeof input.eventId === "string" && input.eventId ? { eventId: input.eventId } : {}),
      ...(typeof input.occurredAtMs === "number" ? { occurredAtMs: input.occurredAtMs } : {}),
      ...(headers === undefined ? {} : { headers }),
      correlationId: optionalString(input.correlationId),
      ...(links === undefined ? {} : { links }),
    });
    return c.json({ event, duplicate }, 202);
  });

  /// A replay is a fresh event reusing the stored document and routing.
  app.post("/:botId/events/replay", requireAction("manageBot"), async (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const { universe, record } = found;
    const body = await readBody(c);
    if (typeof body.seq !== "number") return badRequest(c, "invalid body");
    const stored = record.events.find((event) => event.seq === body.seq);
    if (!stored) return notFound(c);
    if (record.bot.closedAtMs != null) return c.json({ error: "bot is closed" }, 410);
    if (record.bot.enabled === false) return conflict(c, "bot is disabled");
    let summary = `replay of #${stored.seq}`;
    let source = "replay";
    let payload: unknown;
    const raw = store.readText(stored.documentRef);
    if (raw) {
      try {
        const document = asRecord(JSON.parse(raw));
        if (typeof document?.summary === "string") summary = document.summary;
        if (typeof document?.source === "string") source = document.source;
        payload = document?.data;
      } catch {
        // Unreadable document: the envelope stub above stands in.
      }
    }
    try {
      const { event } = admitBotEvent(store, universe, record, {
        eventId: `replay-${crypto.randomUUID()}`,
        kind: stored.kind,
        source,
        summary,
        payload,
        documentRef: stored.documentRef,
        trigger: stored.triggerId ? (record.triggers.get(stored.triggerId) ?? null) : null,
        session: stored.session,
      });
      return c.json({ event }, 202);
    } catch (error) {
      return botErrorResponse(c, error);
    }
  });

  app.get("/:botId/events", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const limit = Math.min(intQuery(c, "limit", 50), MAX_HISTORY_LIMIT);
    const cursor = c.req.query("cursor");
    const newestFirst = [...found.record.events].sort((left, right) => right.seq - left.seq);
    let start = 0;
    if (cursor) {
      const index = newestFirst.findIndex((event) => String(event.seq) === cursor);
      if (index < 0) return badRequest(c, "invalid cursor");
      start = index + 1;
    }
    const events = newestFirst.slice(start, start + limit);
    const last = events.at(-1);
    return c.json({
      events,
      nextCursor: last && start + limit < newestFirst.length ? String(last.seq) : null,
    });
  });

  app.get("/:botId/events/:seq", (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const seq = Number(c.req.param("seq"));
    if (!Number.isSafeInteger(seq)) return badRequest(c, "invalid seq");
    const event = found.record.events.find((entry) => entry.seq === seq);
    if (!event) return notFound(c);
    let document: BotEventDocument = {
      version: 1,
      kind: event.kind,
      source: event.triggerId ?? "manual",
      occurredAtMs: event.occurredAtMs,
      summary: event.summary,
    };
    const raw = store.readText(event.documentRef);
    if (raw) {
      try {
        document = JSON.parse(raw) as BotEventDocument;
      } catch {
        // The reconstructed stub above stands in.
      }
    }
    return c.json({ document, event });
  });

  /// The demo has no CEL evaluator: every sampled event matches, which is
  /// enough for the page to exercise the request/response shapes.
  app.post("/:botId/filters/test", requireAction("manageBot"), async (c) => {
    const found = botFor(store, c);
    if (!found) return notFound(c);
    const body = await readBody(c);
    if (typeof body.filter !== "string" || !body.filter.trim()) {
      return badRequest(c, "validation failed: filter is required");
    }
    if (body.payload !== undefined) {
      const results: BotFilterTestResult[] = [{ matched: true, seq: null }];
      return c.json({ sampled: 1, matched: 1, errors: 0, results });
    }
    const limit = Math.min(typeof body.limit === "number" ? body.limit : 20, MAX_HISTORY_LIMIT);
    const sample = [...found.record.events].sort((left, right) => right.seq - left.seq).slice(0, limit);
    const results: BotFilterTestResult[] = sample.map((event) => ({ matched: true, seq: event.seq }));
    return c.json({ sampled: sample.length, matched: sample.length, errors: 0, results });
  });

  return app;
}
