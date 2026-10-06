/// The in-browser stand-in for the platform backend: one Hono app the fetch
/// shim hands every same-origin `/api/*` request to.
import { Hono } from "hono";
import type { DemoStore } from "./store";
import { adminRoutes } from "./routes/admin";
import { authRoutes } from "./routes/auth";
import { botRoutes, hookRoutes } from "./routes/bots";
import { channelRoutes } from "./routes/channels";
import { environmentRoutes } from "./routes/environments";
import { mcpRoutes } from "./routes/mcp";
import { platformRoutes } from "./routes/platform";
import { profileRoutes } from "./routes/profiles";
import { secretRoutes } from "./routes/secrets";
import { transcriptionRoutes } from "./routes/transcriptions";
import { sessionRoutes } from "./routes/sessions";
import { workspaceRoutes } from "./routes/workspaces";

export function createDemoRouter(store: DemoStore): Hono {
  const app = new Hono();
  app.get("/health", (c) => c.json({ ok: true, demo: true }));
  app.get("/api/login-config", (c) => c.json({ sso: false, providerId: null, password: "local", autoSignIn: false }));
  app.get("/api/v1/admin/audit", (c) => store.currentUser.role === "admin"
    ? c.json(store.auditEvents.slice(-100).reverse()) : c.json({ error: "platform admin required" }, 403));
  app.get("/api/v1/admin/universes/:id/deleted-sessions", (c) => {
    if (store.currentUser.role !== "admin") return c.json({ error: "platform admin required" }, 403);
    const prefix = `${c.req.param("id")}:`;
    const sessions = [...store.deletedSessions.entries()].filter(([key]) => key.startsWith(prefix))
      .map(([, { record, deletedAtMs }]) => ({ sessionId: record.view.id, displayName: record.view.displayName ?? null, deletedAtMs }))
      .sort((a, b) => a.sessionId.localeCompare(b.sessionId)).filter((session) => !c.req.query("after") || session.sessionId > c.req.query("after")!);
    return c.json({ sessions: sessions.slice(0, 100), nextAfter: sessions.length > 100 ? sessions[99]!.sessionId : null });
  });
  app.post("/api/v1/admin/universes/:id/sessions/:sessionId/purge", (c) => {
    if (store.currentUser.role !== "admin") return c.json({ error: "platform admin required" }, 403);
    const universeId = c.req.param("id");
    const sessionId = c.req.param("sessionId");
    if (store.universes.get(universeId)?.sessions.has(sessionId)) return c.json({ error: "session must be soft-deleted first" }, 409);
    const all = [...store.deletedSessions.entries()].filter(([key]) => key.startsWith(`${universeId}:`));
    const ids = new Set<string>();
    if (store.deletedSessions.has(`${universeId}:${sessionId}`)) ids.add(sessionId);
    let previous = -1;
    while (previous !== ids.size) {
      previous = ids.size;
      for (const [, { record }] of all) {
        const view = record.view;
        if ((view.origin?.parentSessionId && ids.has(view.origin.parentSessionId))) ids.add(view.id);
      }
    }
    for (const id of ids) store.deletedSessions.delete(`${universeId}:${id}`);
    if (ids.size) store.auditEvents.push({ id: store.nextId("audit"), universeId, targetId: sessionId,
      actorId: store.currentUser.id, action: "session.purge", outcome: "success", createdAt: new Date().toISOString(), details: { deletedSessionIds: [...ids] } });
    return c.json({ deletedSessionIds: [...ids] });
  });
  app.route("/api/auth", authRoutes(store));
  // The public webhook ingress lives outside /api, exactly like the core's
  // POST /hooks/bots/{universe}/{bot}/{trigger}/{token} route.
  app.route("/hooks", hookRoutes(store));

  const api = new Hono();
  api.route("/", platformRoutes(store));
  api.route("/", adminRoutes(store));
  for (const routes of [
    sessionRoutes,
    transcriptionRoutes,
    profileRoutes,
    workspaceRoutes,
    environmentRoutes,
    mcpRoutes,
    secretRoutes,
    botRoutes,
    channelRoutes,
  ]) {
    api.route("/universes", routes(store));
  }
  app.route("/api/v1", api);

  // A missing stub surfaces as an ordinary API error in the UI instead of a
  // silent hang, which is how gaps get found.
  app.notFound((c) => {
    const what = `${c.req.method} ${new URL(c.req.url).pathname}`;
    console.warn(`[demo] no stub for ${what}`);
    return c.json({ error: `demo: no stub for ${what}` }, 404);
  });
  app.onError((error, c) => {
    console.error("[demo]", error);
    return c.json({ error: `demo stub failed: ${error.message}` }, 500);
  });
  return app;
}
