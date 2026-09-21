import { Hono } from "hono";
import { z } from "zod";
import { eq, and } from "drizzle-orm";
import { hashPassword } from "better-auth/crypto";
import { schema } from "@lightspeed/platform-db";
import type { AccessChange, AccessDirectory } from "@lightspeed-ai/agent-client";
import type { AppContext, ApiVariables } from "../context.js";
import { isPlatformAdmin } from "../context.js";
import { parseBody } from "../http.js";
import { deploymentClientFor, withGateway } from "./gateway.js";

const profile = z.object({
  name: z.string().trim().min(1).max(120), email: z.email().transform((s) => s.toLowerCase()),
  role: z.enum(["user", "admin"]),
});
const createUser = profile.extend({ password: z.string().min(8).max(128) });
const updateUser = profile.partial().extend({ emailVerified: z.literal(true).optional(), status: z.enum(["active", "disabled"]).optional() });
const password = z.object({ newPassword: z.string().min(8).max(128) });

function deploymentRole(directory: AccessDirectory, id: string) {
  const groups = new Set(directory.memberships.filter((m) => m.principalId === id).map((m) => m.groupId));
  return directory.roles.some((r) => r.scope.kind === "deployment" && r.role === "deployment_admin" &&
    (r.subject.kind === "principal" ? r.subject.id === id : groups.has(r.subject.id))) ? "admin" : "user";
}

export function identityRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.use("*", async (c, next) => {
    if (!isPlatformAdmin()) return c.json({ error: "deployment admin required" }, 403);
    await next();
  });
  app.get("/identity", (c) => withGateway(c, async () => {
    const result = await deploymentClientFor(ctx).call("deployment/identity/directory", { scope: { kind: "deployment" } });
    return c.json(result.result);
  }));
  // Core validates this discriminated contract and authorizes each operation.
  app.post("/identity", (c) => withGateway(c, async () => {
    const change = await c.req.json<AccessChange>();
    const result = await deploymentClientFor(ctx).call("deployment/identity/apply", change);
    return c.json(result.result);
  }));
  app.get("/users", (c) => withGateway(c, async () => {
    const directory = (await deploymentClientFor(ctx).call("deployment/identity/directory", { scope: { kind: "deployment" } })).result;
    const users = await ctx.db.select().from(schema.user);
    return c.json({ users: users.map((u) => ({ ...u, role: deploymentRole(directory, u.corePrincipalId),
      directRole: directory.roles.some((r) => r.scope.kind === "deployment" && r.subject.kind === "principal" && r.subject.id === u.corePrincipalId && r.role === "deployment_admin") ? "admin" : "user",
      status: directory.principals.find((p) => p.id === u.corePrincipalId)?.status ?? "disabled" })) });
  }));
  app.post("/users", (c) => withGateway(c, async () => {
    const body = await parseBody(c, createUser);
    if (!body.ok) return body.response;
    const id = crypto.randomUUID();
    const corePrincipalId = crypto.randomUUID();
    const client = deploymentClientFor(ctx);
    // Reserve a new canonical identity; failed account creation leaves no login
    // linked to it. Never resolve or adopt an identity by email/display name.
    const encoded = await hashPassword(body.data.password);
    const created = await ctx.db.transaction(async (tx) => {
      const [user] = await tx.insert(schema.user).values({ id, corePrincipalId,
        name: body.data.name, email: body.data.email, emailVerified: true }).returning();
      await tx.insert(schema.account).values({ id: crypto.randomUUID(), accountId: id, providerId: "credential", userId: id, password: encoded });
      await client.call("deployment/identity/apply", { operation: "create_principal", id: corePrincipalId, kind: "user", displayName: body.data.name, managementScope: { kind: "deployment" } });
      if (body.data.role === "admin") await client.call("deployment/identity/apply", { operation: "assign_role", assignment: { scope: { kind: "deployment" }, subject: { kind: "principal", id: corePrincipalId }, role: "deployment_admin" } });
      return user;
    });
    return c.json({ user: { ...created, role: body.data.role } }, 201);
  }));
  app.patch("/users/:id", (c) => withGateway(c, async () => {
    const body = await parseBody(c, updateUser);
    if (!body.ok) return body.response;
    const [target] = await ctx.db.select().from(schema.user).where(eq(schema.user.id, c.req.param("id"))).limit(1);
    if (!target) return c.json({ error: "user not found" }, 404);
    const client = deploymentClientFor(ctx);
    if (body.data.role) {
      await client.call("deployment/identity/apply", { operation: body.data.role === "admin" ? "assign_role" : "revoke_role",
        assignment: { scope: { kind: "deployment" }, subject: { kind: "principal", id: target.corePrincipalId }, role: "deployment_admin" } });
    }
    if (body.data.status) {
      await client.call("deployment/identity/apply", { operation: "set_principal_status", id: target.corePrincipalId, status: body.data.status });
    }
    const { role: _role, status: _status, ...data } = body.data;
    const [updated] = await ctx.db.update(schema.user).set({ ...data, updatedAt: new Date() }).where(eq(schema.user.id, target.id)).returning();
    if (body.data.status === "disabled") await ctx.db.delete(schema.session).where(eq(schema.session.userId, target.id));
    return c.json({ user: updated });
  }));
  app.post("/users/:id/password", (c) => withGateway(c, async () => {
    const body = await parseBody(c, password);
    if (!body.ok) return body.response;
    const encoded = await hashPassword(body.data.newPassword);
    const found = await ctx.db.transaction(async (tx) => {
      const rows = await tx.update(schema.account).set({ password: encoded, updatedAt: new Date() }).where(and(eq(schema.account.userId, c.req.param("id")), eq(schema.account.providerId, "credential"))).returning({ id: schema.account.id });
      if (rows.length) await tx.delete(schema.session).where(eq(schema.session.userId, c.req.param("id")));
      return rows.length > 0;
    });
    return found ? c.json({ ok: true }) : c.json({ error: "password account not found" }, 404);
  }));
  return app;
}
