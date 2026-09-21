import { Hono } from "hono";
import type { AccessReadParams, AccessReadResponse, UniverseAction } from "@lightspeed-ai/agent-client";
import type { DemoStore } from "../store";
import { notFound, readBody, universeFor } from "./common";

/** Scripted demo ownership: its sessions, profiles and bots belong to the demo user. */
export function accessRoutes(store: DemoStore): Hono {
  const app = new Hono();
  app.post("/:id/access", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const role = universe.universe.role;
    if (!role) return c.json({ error: "Universe access required" }, 403);
    const body = await readBody<AccessReadParams>(c);
    const contribute = ["contributor", "operator", "admin"].includes(role);
    const configure = ["operator", "admin"].includes(role);
    const actions: UniverseAction[] = ["read"];
    if (contribute) actions.push("create_session", "create_profile", "create_bot", "use_resource");
    if (configure) actions.push("configure_resource");
    if (role === "admin") actions.push("manage_access");
    const response: AccessReadResponse = {
      actions,
      resources: (body.resources ?? []).map((resource) => {
        const exists = resource.kind === "session" ? universe.sessions.has(resource.id)
          : resource.kind === "bot" ? universe.bots.has(resource.id) : universe.profiles.has(resource.id);
        const allowed: UniverseAction[] = exists ? ["read"] : [];
        if (exists && contribute) {
          if (resource.kind === "session") allowed.push("control_session", "stop_session", "delete_session");
          if (resource.kind === "bot") allowed.push("manage_bot", "invoke_bot");
          if (resource.kind === "profile") allowed.push("manage_profile");
        }
        return { resource, actions: allowed };
      }),
    };
    return c.json(response);
  });
  return app;
}
