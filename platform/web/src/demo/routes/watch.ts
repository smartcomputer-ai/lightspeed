import { Hono } from "hono";
import type { AgentNotification } from "@lightspeed-ai/sdk";
import { sessionSummary, type DemoStore, type UniverseState } from "../store";
import { listItemOf } from "./bots";

/** The demo has no database change bus. Compare its small in-memory projections
 * on a short timer so scripted background activity uses the same browser stream.
 */
export function watchRoutes(store: DemoStore): Hono {
  const app = new Hono();
  app.get("/:id/watch", c => {
    const universe = store.universe(c.req.param("id"));
    if (!universe) return c.json({ error: "not found" }, 404);
    const encoder = new TextEncoder();
    let previous = snapshot(store, universe);
    let timer: ReturnType<typeof setInterval>;
    let close: () => void = () => {};
    let cancel: () => void = () => {};
    const body = new ReadableStream<Uint8Array>({
      start(output) {
        let closed = false;
        cancel = () => {
          if (closed) return;
          closed = true; clearInterval(timer);
          c.req.raw.signal.removeEventListener("abort", close);
        };
        close = () => { if (!closed) { cancel(); output.close(); } };
        const send = (event: AgentNotification) => output.enqueue(encoder.encode(`data: ${JSON.stringify(event)}\n\n`));
        send({ method: "watch/reset", params: {} });
        let ticks = 0;
        let missed = false;
        timer = setInterval(() => {
          if (c.req.raw.signal.aborted) { close(); return; }
          const current = snapshot(store, universe);
          if ((output.desiredSize ?? 0) <= 0) { missed = true; return; }
          if (missed) {
            send({ method: "watch/reset", params: {} });
            previous = current; missed = false; return;
          }
          for (const [key, item] of current) {
            if (previous.get(key)?.serialized !== item.serialized) send(item.event);
          }
          for (const [key, item] of previous) {
            if (!current.has(key)) {
              const event = item.event;
              if (event.method === "session/changed") send({ method: "session/removed", params: { sessionId: event.params.session.id } });
              if (event.method === "bots/changed") send({ method: "bots/removed", params: { botId: event.params.bot.botId } });
              if (event.method === "environments/changed") send({ method: "environments/removed", params: { environmentId: event.params.environment.environmentId } });
              if (event.method === "workspaces/changed") send({ method: "workspaces/removed", params: { workspaceId: event.params.workspace.workspaceId } });
            }
          }
          previous = current;
          if (++ticks % 60 === 0) output.enqueue(encoder.encode(": keepalive\n\n"));
        }, 250);
        c.req.raw.signal.addEventListener("abort", close, { once: true });
        if (c.req.raw.signal.aborted) close();
      },
      cancel() { cancel(); },
    });
    return new Response(body, { headers: { "content-type": "text/event-stream", "cache-control": "no-store" } });
  });
  return app;
}

function snapshot(store: DemoStore, universe: UniverseState) {
  const rows = new Map<string, { event: AgentNotification; serialized: string }>();
  const add = (key: string, event: AgentNotification) => rows.set(key, { event, serialized: JSON.stringify(event) });
  const admin = store.currentUser.role === "admin" || universe.universe.role === "admin";
  for (const record of universe.sessions.values()) {
    const root = universe.sessions.get(record.view.origin?.rootSessionId ?? record.view.id) ?? record;
    const access = root.view.access;
    if (!admin && access.visibility !== "universe" && !(access.createdBy?.kind === "actor" && access.createdBy.id === store.currentUser.id)) continue;
    add(`session:${record.view.id}`, { method: "session/changed", params: { session: {
      ...sessionSummary(record), access, retention: root.view.retention, lifecycleControlled: root.view.lifecycleControlled,
    } } });
  }
  if (universe.universe.features?.bots) for (const record of universe.bots.values()) {
    add(`bot:${record.bot.botId}`, { method: "bots/changed", params: { bot: listItemOf(universe, record) } });
  }
  for (const environment of universe.environments.values()) add(`environment:${environment.environmentId}`, {
    method: "environments/changed", params: { environment },
  });
  for (const entry of universe.workspaces.values()) add(`workspace:${entry.row.workspaceId}`, {
    method: "workspaces/changed", params: { workspace: entry.row },
  });
  return rows;
}
