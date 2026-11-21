import { Hono } from "hono";
import type { WebhookPreset } from "@lightspeed-ai/sdk";
import { admitBotEvent, botErrorResponse, webhookTokenOf } from "../bot-runtime";
import { asRecord } from "../input";
import type { DemoStore, UniverseState } from "../store";
import { conflict, notFound } from "./common";

const MAX_BODY_BYTES = 1024 * 1024;

interface WebhookExtraction {
  eventId: string;
  kind: string;
  summary: string;
  promptData?: unknown;
}

/// The GitHub preset projects the envelope (action, repository, sender, the
/// subject named after the event); plain webhooks take `kind` from the body.
function extractWebhookEvent(
  triggerId: string,
  preset: WebhookPreset | null,
  data: unknown,
  header: (name: string) => string | undefined,
): WebhookExtraction {
  const body = asRecord(data);
  if (preset === "github") {
    const ghEvent = header("x-github-event") ?? "unknown";
    const action = typeof body?.action === "string" ? body.action : null;
    const kind = action ? `${ghEvent}.${action}` : ghEvent;
    const repoName = asRecord(body?.repository)?.full_name;
    const repository = typeof repoName === "string" ? repoName : null;
    const subject = asRecord(body?.[ghEvent]);
    const promptData =
      subject === undefined
        ? data
        : {
            ...(action === null ? {} : { action }),
            ...(repository === null ? {} : { repository }),
            ...(body?.sender === undefined ? {} : { sender: body.sender }),
            [ghEvent]: subject,
          };
    return {
      eventId: header("x-github-delivery") ?? crypto.randomUUID(),
      kind,
      summary: `GitHub ${kind}${repository ? ` in ${repository}` : ""}`,
      promptData,
    };
  }
  const kind = typeof body?.kind === "string" && body.kind ? body.kind.slice(0, 200) : "webhook";
  return {
    // A digest id would dedupe the page's identical sample bodies; a uuid
    // lets every test-fire show up.
    eventId: crypto.randomUUID(),
    kind,
    summary: `Webhook ${kind} received on trigger ${triggerId}`,
  };
}

/// The `:universe` path segment resolves by the core universe id first
/// (what ingest paths carry), then by the platform id.
function universeByAnyId(store: DemoStore, id: string): UniverseState | null {
  for (const state of store.universes.values()) {
    if (state.universe.lightspeedUniverseId === id) return state;
  }
  return store.universe(id);
}

/// Public ingress: `POST /ingress/webhooks/bots/{universe}/{bot}/{trigger}/{token}`,
/// authenticated by the per-trigger URL token alone. A wrong token is
/// indistinguishable from an unknown endpoint, like the core's route.
export function hookRoutes(store: DemoStore): Hono {
  const app = new Hono();
  app.onError((error, c) => botErrorResponse(c, error));

  app.post("/bots/:universeId/:botId/:triggerId/:token", async (c) => {
    const universe = universeByAnyId(store, c.req.param("universeId") ?? "");
    const record = universe?.bots.get(c.req.param("botId") ?? "");
    const trigger = record?.triggers.get(c.req.param("triggerId") ?? "");
    if (!universe || !record || !trigger || trigger.kind !== "webhook") return notFound(c);
    const token = webhookTokenOf(trigger);
    if (!token || token !== c.req.param("token")) return notFound(c);
    const raw = await c.req.text();
    if (raw.length > MAX_BODY_BYTES) return c.json({ error: "payload too large" }, 413);
    // A closed bot is gone for good: tell the sender to stop, not to retry.
    if (record.bot.closedAtMs != null) return c.json({ error: "bot is closed" }, 410);
    if (record.bot.enabled === false || trigger.enabled === false) {
      return conflict(c, "trigger is disabled");
    }
    let data: unknown;
    try {
      data = raw ? JSON.parse(raw) : undefined;
    } catch {
      data = undefined;
    }
    const extraction = extractWebhookEvent(trigger.triggerId, trigger.preset ?? null, data, (name) =>
      c.req.header(name),
    );
    const { event, duplicate } = admitBotEvent(store, universe, record, {
      eventId: extraction.eventId,
      kind: extraction.kind,
      source: `webhook:${trigger.triggerId}`,
      summary: extraction.summary,
      payload: data,
      ...(extraction.promptData === undefined ? {} : { promptData: extraction.promptData }),
      trigger,
    });
    return c.json({ eventId: event.eventId, duplicate }, 202);
  });

  return app;
}
