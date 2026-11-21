import {
  auditQuerySchema, bindModuleOperations, modulePermissions, validateModuleDefinitions,
  type AuditPage, type ModuleOperation, type ModuleRouteEntries, type PlatformModule,
} from "@lightspeed-ai/platform-shared";
import { demoDeploymentAllows, demoModuleEnabled, demoUniverseActions } from "./access";
import { organizationRoutes } from "./routes/organizations";
import { watchRoutes } from "./routes/watch";
/// The in-browser stand-in for the platform backend: one Hono app the fetch
/// shim hands every same-origin `/api/*` request to.
import { Hono, type MiddlewareHandler } from "hono";
import type { DemoStore } from "./store";
import { adminRoutes } from "./routes/admin";
import { authRoutes } from "./routes/auth";
import { hookRoutes } from "./routes/bot-webhooks";
import { environmentRoutes } from "./routes/environments";
import { mcpRoutes } from "./routes/mcp";
import { platformRoutes } from "./routes/platform";
import { profileRoutes } from "./routes/profiles";
import { secretRoutes } from "./routes/secrets";
import { transcriptionRoutes } from "./routes/transcriptions";
import { conversationRoutes } from "./routes/sessions";
import { workspaceRoutes } from "./routes/workspaces";

export type DemoRoutes = ModuleRouteEntries<(store: DemoStore) => Hono>;
export type DemoModule = PlatformModule & DemoRoutes;

export function createDemoRouter(store: DemoStore, modules: readonly DemoModule[] = []): Hono {
  validateModuleDefinitions(modules);
  const app = new Hono();
  app.get("/health", (c) => c.json({ ok: true, demo: true }));
  app.get("/api/login-config", (c) => c.json({ sso: false, providerId: null, password: "local", autoSignIn: false }));
  app.get("/api/ui/admin/audit", (c) => {
    if (!demoDeploymentAllows(store, "readAudit")) return c.json({ error: "deployment permission required: readAudit" }, 403);
    const query = auditQuerySchema.safeParse(c.req.query());
    if (!query.success) return c.json({ error: "invalid audit pagination" }, 400);
    const { limit, cursor } = query.data;
    const ordered = [...store.auditEvents].sort((a, b) => b.createdAt.localeCompare(a.createdAt) || b.id.localeCompare(a.id));
    const anchor = cursor ? ordered.findIndex((event) => event.id === cursor) : -1;
    if (cursor && anchor === -1) return c.json({ error: "audit cursor not found" }, 400);
    const rows = ordered.slice(anchor + 1, anchor + limit + 2);
    const events = rows.slice(0, limit);
    const page: AuditPage = { events, nextCursor: rows.length > limit ? events.at(-1)!.id : null };
    return c.json(page);
  });
  app.get("/api/ui/admin/universes/:id/deleted-sessions", (c) => {
    if (!demoDeploymentAllows(store, "purgeSessions")) return c.json({ error: "deployment permission required: purgeSessions" }, 403);
    const prefix = `${c.req.param("id")}:`;
    const sessions = [...store.deletedSessions.entries()].filter(([key]) => key.startsWith(prefix))
      .map(([, { record, deletedAtMs }]) => ({ sessionId: record.view.id, displayName: record.view.displayName ?? null, deletedAtMs }))
      .sort((a, b) => a.sessionId.localeCompare(b.sessionId)).filter((session) => !c.req.query("after") || session.sessionId > c.req.query("after")!);
    return c.json({ sessions: sessions.slice(0, 100), nextAfter: sessions.length > 100 ? sessions[99]!.sessionId : null });
  });
  app.post("/api/ui/admin/universes/:id/sessions/:sessionId/purge", (c) => {
    if (!demoDeploymentAllows(store, "purgeSessions")) return c.json({ error: "deployment permission required: purgeSessions" }, 403);
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
    if (ids.size) store.auditEvents.push({ id: crypto.randomUUID(), universeId, targetId: sessionId,
      actorId: store.currentUser.id, action: "session.purge", outcome: "success", createdAt: new Date().toISOString(), details: { deletedSessionIds: [...ids] } });
    return c.json({ deletedSessionIds: [...ids] });
  });
  app.route("/api/auth", authRoutes(store));
  // The public webhook ingress lives outside /api, exactly like the core's
  // POST /ingress/webhooks/bots/{universe}/{bot}/{trigger}/{token} route.
  app.route("/ingress/webhooks", hookRoutes(store));

  const api = new Hono();
  api.route("/", platformRoutes(store));
  api.route("/", adminRoutes(store));
  for (const routes of [
    conversationRoutes,
    transcriptionRoutes,
    profileRoutes,
    workspaceRoutes,
    environmentRoutes,
    mcpRoutes,
    secretRoutes,
  ]) {
    api.route("/universes", routes(store));
  }
  for (const module of modules) {
    const admit = (operation?: ModuleOperation): MiddlewareHandler => async (c, next) => {
      const state = store.universe(c.req.param("id"));
      if (!state) return c.json({ error: "not found" }, 404);
      const actions = demoUniverseActions(store, state);
      if (!actions.has("read")) return c.json({ error: "not found" }, 404);
      if (!demoModuleEnabled(store, state, module)) return c.json({ error: "module unavailable" }, 404);
      if (module.requiresFeature && !state.universe.features[module.requiresFeature]) {
        return c.json({ error: `${module.name} are turned off in this universe` }, 403);
      }
      if (operation && !modulePermissions(module, actions).has(operation.permission)) {
        return c.json({ error: `Permission required: ${module.id}/${operation.permission}` }, 403);
      }
      await next();
    };
    const base = `/universes/:id/modules/${module.id}`;
    if (module.browserRoutes) {
      const scoped = new Hono().use("*", admit());
      scoped.route("/", module.browserRoutes(store));
      api.route(base, scoped);
    }
    if (module.publicOperations) {
      if (!module.operations?.length) throw new Error(`Module ${module.id} provides a public API implementation without declared operations`);
      const scoped = new Hono();
      // Wrapping the contributed router preserves its own error handler.
      const router = new Hono().route("/", module.publicOperations(store));
      for (const { operation, routes } of bindModuleOperations(module.id, module.operations, router.routes)) {
        scoped.on(operation.method, operation.path, admit(operation), ...routes.map(route => route.handler));
      }
      app.route(`/api/v1${base}`, scoped);
    }
  }
  app.route("/api/ui", api);
  app.route("/api/v1/platform", organizationRoutes(store, modules));
  app.route("/api/v1/universes", watchRoutes(store, modules));

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
