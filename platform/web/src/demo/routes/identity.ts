import { Hono } from "hono";
import type { AccessDirectory, AccessChange } from "@lightspeed-ai/agent-client";
import type { DemoStore, DemoUser } from "../store";
import { nowIso, readBody } from "./common";

const directories = new WeakMap<DemoStore, AccessDirectory>();
export function directoryFor(store: DemoStore): AccessDirectory {
  let directory = directories.get(store);
  if (!directory) {
    directory = { principals: [...store.users.values()].map((u) => ({ id: u.id, displayName: u.name, kind: "user", status: "active", managementScope: { kind: "deployment" }, createdAtMs: Date.parse(u.createdAt) })), groups: [], memberships: [], roles: [], capabilities: [], policyRevision: 0 };
    directories.set(store, directory);
  }
  return directory;
}
export function identityRoutes(store: DemoStore): Hono {
  const app = new Hono();
  app.get("/admin/identity", (c) => c.json(directoryFor(store)));
  app.post("/admin/identity", async (c) => {
    const change = await readBody<AccessChange>(c);
    const d = directoryFor(store);
    switch (change.operation) {
      case "create_group": d.groups.push({ id: change.id, displayName: change.displayName, createdAtMs: Date.now() }); break;
      case "rename_group": { const g = d.groups.find((g) => g.id === change.id); if (g) g.displayName = change.displayName; break; }
      case "put_membership": if (!d.memberships.some((m) => JSON.stringify(m) === JSON.stringify(change.membership))) d.memberships.push(change.membership); break;
      case "remove_membership": d.memberships = d.memberships.filter((m) => m.groupId !== change.membership.groupId || m.principalId !== change.membership.principalId); break;
      case "assign_role": d.roles.push(change.assignment); break;
      case "revoke_role": d.roles = d.roles.filter((r) => JSON.stringify(r) !== JSON.stringify(change.assignment)); break;
      default: return c.json({ error: "demo: unsupported identity operation" }, 400);
    }
    d.policyRevision++;
    return c.json({ changed: true, policyRevision: d.policyRevision });
  });
  app.get("/admin/users", (c) => {
    const users = [...store.users.values()];
    return c.json({ users, total: users.length, limit: users.length, offset: 0 });
  });
  app.post("/admin/users", async (c) => {
    const body = await readBody<{ email?: string; name?: string; role?: string }>(c);
    if (!body.email?.trim()) return c.json({ message: "email is required" }, 400);
    const at = nowIso();
    const user: DemoUser = {
      id: store.nextId("user"),
      name: body.name?.trim() || body.email.split("@")[0] || "New user",
      email: body.email.trim(),
      role: body.role ?? "user",
      emailVerified: true,
      image: null,
      banned: false,
      createdAt: at,
      updatedAt: at,
    };
    store.users.set(user.id, user);
    return c.json({ user });
  });
  app.patch("/admin/users/:id", async (c) => {
    const body = await readBody<{
      userId?: string;
      data?: {
        name?: unknown;
        email?: unknown;
        emailVerified?: unknown;
        role?: unknown;
      };
    }>(c);
    const target = store.users.get(c.req.param("id"));
    if (!target) return c.json({ message: "user not found" }, 404);
    const data = body as { name?: unknown; email?: unknown; emailVerified?: unknown; role?: unknown; status?: unknown };
    if (typeof data.name === "string" && data.name.trim()) {
      target.name = data.name.trim();
    }
    if (typeof data.email === "string" && data.email.trim()) {
      const email = data.email.trim().toLowerCase();
      if ([...store.users.values()].some((user) => user.id !== target.id && user.email === email)) {
        return c.json({ message: "user already exists; use another email" }, 400);
      }
      target.email = email;
    }
    if (typeof data.emailVerified === "boolean") target.emailVerified = data.emailVerified;
    if (data.role === "user" || data.role === "admin") target.role = data.role;
    target.updatedAt = nowIso();
    return c.json(target);
  });
  app.post("/admin/users/:id/password", async (c) => {
    const body = await readBody<{ userId?: string; newPassword?: string }>(c);
    if (!store.users.has(c.req.param("id"))) {
      return c.json({ message: "user not found" }, 404);
    }
    if (!body.newPassword || body.newPassword.length < 8) {
      return c.json({ message: "password is too short" }, 400);
    }
    return c.json({ status: true });
  });
  return app;
}
