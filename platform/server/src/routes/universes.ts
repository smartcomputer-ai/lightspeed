import { Hono } from "hono";
import { z } from "zod";
import { eq } from "drizzle-orm";
import { LightspeedRpcError } from "@lightspeed-ai/agent-client";
import { schema } from "@lightspeed/platform-db";
import {
  memberAddSchema,
  memberUpdateSchema,
  slugify,
  universeCreateSchema,
  universeUpdateSchema,
} from "@lightspeed/platform-shared";
import type { AppContext, ApiVariables } from "../context.js";
import { isPlatformAdmin } from "../context.js";
import { parseBody } from "../http.js";
import { engineClientFor, deploymentClientFor, withGateway } from "./gateway.js";

const { universes, user } = schema;
type UniverseRow = typeof universes.$inferSelect;

export function highestRole(roles: string[]): string | null {
  return ["admin", "operator", "contributor", "viewer"].find((r) => roles.includes(r)) ?? null;
}

async function universeForSession(
  ctx: AppContext,
  _c: { get: (key: "session") => ApiVariables["session"] },
  universeId: string,
): Promise<{ universe: UniverseRow; slug: string; role: string } | null> {
  const [universe] = await ctx.db.select().from(universes).where(eq(universes.id, universeId)).limit(1);
  if (!universe) return null;
  const response = await deploymentClientFor(ctx).call("deployment/identity/self", {
    scope: { kind: "universe", universeId: universe.lightspeedUniverseId },
  });
  const role = highestRole(response.result.access.roles);
  return role ? { universe, slug: universe.slug, role } : null;
}

async function createUniverseRows(ctx: AppContext, name: string, baseSlug: string, lightspeedUniverseId: string) {
  for (let suffix = 1; ; suffix++) {
    const slug = suffix === 1 ? baseSlug : `${baseSlug}-${suffix}`;
    const [universe] = await ctx.db.insert(universes).values({ name, slug, lightspeedUniverseId })
      .onConflictDoNothing({ target: universes.slug }).returning();
    if (universe) return universe;
  }
}

const adoptSchema = z.object({
  // Shape only, not RFC 4122: the id comes from the engine, which uses
  // non-conformant ids like the …0001 default universe (version nibble 0).
  lightspeedUniverseId: z
    .string()
    .regex(/^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/),
  name: z.string().trim().min(1).max(120),
});

const apiKeyCreateSchema = z.object({
  principalId: z.string().uuid(),
  displayName: z.string().trim().min(1).max(120),
});

export function universeRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  app.get("/", (c) => withGateway(c, async () => {
    const self = await deploymentClientFor(ctx).call("deployment/identity/self", { scope: { kind: "deployment" } });
    const rights = new Map(self.result.universes.map((r) => [r.scope.kind === "universe" ? r.scope.universeId : "", highestRole(r.roles)]));
    const rows = await ctx.db.select().from(universes);
    return c.json(rows.filter((r) => rights.has(r.lightspeedUniverseId) || isPlatformAdmin()).map((r) => ({ ...r, role: rights.get(r.lightspeedUniverseId) ?? null })));
  }));

  app.post("/", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin()) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const body = await parseBody(c, universeCreateSchema);
    if (!body.ok) {
      return body.response;
    }
    const input = body.data;
    const baseSlug = input.slug ?? slugify(input.name);
    // Auto-create is retired engine-side: the universe must exist before
    // anything addresses it, so the engine create comes first. Idempotent
    // — if the platform transaction below fails, the orphaned engine
    // universe is empty, harmless, and reused on retry-by-name (fresh id)
    // or reaped by an deployment purge.
    const lightspeedUniverseId = crypto.randomUUID();
    return withGateway(c, async () => {
      await deploymentClientFor(ctx).call("deployment/universes/create", {
        universeId: lightspeedUniverseId,
      });
      const created = await createUniverseRows(
        ctx,
        input.name,
        baseSlug,
        lightspeedUniverseId,
      );
      return c.json({ ...created, role: "admin" }, 201);
    });
  });

  /// Reconciliation view (platform admin): every platform row checked
  /// against the default deployment's engine inventory, plus the engine
  /// universes no row links to (orphans — CLI/test tenants, or survivors
  /// of a purged platform DB). Rows with a custom gatewayUrl live on
  /// another deployment and are not checked here.
  app.get("/reconcile", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin()) {
      return c.json({ error: "platform admin required" }, 403);
    }
    return withGateway(c, async () => {
      const rows = await ctx.db.select().from(universes);
      const listed = await deploymentClientFor(ctx).call("deployment/universes/list", {});
      const engineViews = listed.result.universes ?? [];
      const engineIds = new Set(engineViews.map((view) => view.universeId));
      const linked = new Set(rows.map((row) => row.lightspeedUniverseId));
      return c.json({
        platform: rows.map((row) => ({
          id: row.id,
          lightspeedUniverseId: row.lightspeedUniverseId,
          engine: row.gatewayUrl
            ? "unchecked"
            : engineIds.has(row.lightspeedUniverseId)
              ? "ok"
              : "missing",
        })),
        orphans: engineViews.filter((view) => !linked.has(view.universeId)),
      });
    });
  });

  /// Adopts an engine universe the platform has no row for: verifies it
  /// exists engine-side (fail closed on typos), then creates the platform
  /// display entry. Core access assignments are unchanged.
  app.post("/adopt", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin()) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const body = await parseBody(c, adoptSchema);
    if (!body.ok) {
      return body.response;
    }
    const input = body.data;
    const [existing] = await ctx.db
      .select({ id: universes.id })
      .from(universes)
      .where(eq(universes.lightspeedUniverseId, input.lightspeedUniverseId))
      .limit(1);
    if (existing) {
      return c.json({ error: "already linked to a universe" }, 409);
    }
    return withGateway(c, async () => {
      await deploymentClientFor(ctx).call("deployment/universes/read", {
        universeId: input.lightspeedUniverseId,
      });
      const created = await createUniverseRows(
        ctx,
        input.name,
        slugify(input.name),
        input.lightspeedUniverseId,
      );
      return c.json({ ...created, role: null }, 201);
    });
  });

  /// Repair for a phantom row (platform knows it, the engine does not —
  /// dev drift from backend swaps or engine resets): re-materializes the
  /// tenant. Safe because engine universes are born empty and create is
  /// idempotent; whatever data made it a phantom was already gone.
  app.post("/:id/engine", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin()) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const [universe] = await ctx.db.select().from(universes).where(eq(universes.id, c.req.param("id"))).limit(1);
    const access = universe ? { universe } : null;
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const created = await deploymentClientFor(ctx, access.universe.gatewayUrl).call(
        "deployment/universes/create",
        { universeId: access.universe.lightspeedUniverseId },
      );
      return c.json({ created: created.result.created });
    });
  });

  /// Universe API-key management stays deployment-scoped engine-side: the platform
  /// asserts the human in the selected universe; core evaluates issuance/ownership. The plaintext secret exists only in the create
  /// response and is never persisted by the platform.
  app.get("/:id/key-principals", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) return c.json({ error: "not found" }, 404);
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const scope = { kind: "universe" as const, universeId: access.universe.lightspeedUniverseId };
      const self = await client.call("deployment/identity/self", { scope });
      const principals = [self.result.access.principal];
      if (self.result.access.roles.includes("admin")) {
        const directory = await client.call("deployment/identity/directory", { scope });
        principals.push(...directory.result.principals.filter((principal) =>
          principal.kind === "service" && principal.status === "active"
          && principal.managementScope.kind === "universe"
          && principal.managementScope.universeId === scope.universeId,
        ));
      }
      return c.json(principals.map(({ id, displayName, kind }) => ({ id, displayName, kind })));
    });
  });

  app.get("/:id/api-keys", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const response = await engineClientFor(ctx, access.universe).call(
        "deployment/api-keys/list",
        { scope: { kind: "universe", universeId: access.universe.lightspeedUniverseId }, },
      );
      return c.json(response.result.apiKeys ?? []);
    });
  });

  app.post("/:id/api-keys", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, apiKeyCreateSchema);
    if (!body.ok) {
      return body.response;
    }
    const session = c.get("session");
    return withGateway(c, async () => {
      const response = await engineClientFor(ctx, access.universe).call(
        "deployment/api-keys/create",
        {
          scope: { kind: "universe", universeId: access.universe.lightspeedUniverseId },
          displayName: body.data.displayName,
          principalId: body.data.principalId,
        },
      );
      return c.json(response.result, 201);
    });
  });

  app.delete("/:id/api-keys/:keyPrefix", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const response = await engineClientFor(ctx, access.universe).call(
        "deployment/api-keys/revoke",
        {
          scope: { kind: "universe", universeId: access.universe.lightspeedUniverseId },
          keyPrefix: c.req.param("keyPrefix"),
        },
      );
      return c.json(response.result.apiKey);
    });
  });

  /// Deletes an engine universe that has no platform row (orphan sweep —
  /// purges its sessions and blobs). Linked universes must go through
  /// archive → purge instead so platform state comes along.
  app.delete("/engine/:lightspeedUniverseId", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin()) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const lightspeedUniverseId = c.req.param("lightspeedUniverseId");
    const [linked] = await ctx.db
      .select({ id: universes.id })
      .from(universes)
      .where(eq(universes.lightspeedUniverseId, lightspeedUniverseId))
      .limit(1);
    if (linked) {
      return c.json({ error: "linked to a universe — archive and delete it instead" }, 409);
    }
    return withGateway(c, async () => {
      const report = await deploymentClientFor(ctx).call("deployment/universes/delete", {
        universeId: lightspeedUniverseId,
      });
      return c.json({ ok: true, purge: report.result });
    });
  });

  /// Permanent removal: engine purge (deployment scope) then the platform
  /// metadata rows and setup installations. Gated to
  /// platform admins and archived universes — archive first, purge second,
  /// so no traffic races the purge.
  app.delete("/:id", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin()) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const [universe] = await ctx.db.select().from(universes).where(eq(universes.id, c.req.param("id"))).limit(1);
    const access = universe ? { universe } : null;
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    if (access.universe.status !== "archived") {
      return c.json({ error: "archive the universe before deleting it" }, 409);
    }
    return withGateway(c, async () => {
      let purge: unknown = null;
      try {
        const report = await deploymentClientFor(ctx, access.universe.gatewayUrl).call(
          "deployment/universes/delete",
          { universeId: access.universe.lightspeedUniverseId },
        );
        purge = report.result;
      } catch (error) {
        // Already gone engine-side (or never materialized) — the platform
        // rows still need to go.
        if (!(error instanceof LightspeedRpcError) || error.kind !== "not_found") {
          throw error;
        }
      }
      await ctx.db
        .delete(universes)
        .where(eq(universes.id, access.universe.id));
      return c.json({ ok: true, purge });
    });
  });

  app.get("/:id", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return c.json({ ...access.universe, slug: access.slug, role: access.role });
  });

  app.patch("/:id", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, universeUpdateSchema);
    if (!body.ok) {
      return body.response;
    }
    if (access.role !== "admin") return c.json({ error: "universe admin required" }, 403);
    const [updated] = await ctx.db.update(universes).set(body.data).where(eq(universes.id, access.universe.id)).returning();
    return c.json({ ...updated, slug: access.slug, role: access.role });
  });

  app.get("/:id/groups", (c) => withGateway(c, async () => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access || access.role !== "admin") return c.json({ error: "universe admin required" }, 403);
    const directory = await deploymentClientFor(ctx).call("deployment/identity/directory", { scope: { kind: "universe", universeId: access.universe.lightspeedUniverseId } });
    return c.json(directory.result.groups);
  }));

  app.get("/:id/members", (c) => withGateway(c, async () => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access || access.role !== "admin") return c.json({ error: "universe admin required" }, 403);
    const directory = await deploymentClientFor(ctx).call("deployment/identity/directory", {
      scope: { kind: "universe", universeId: access.universe.lightspeedUniverseId },
    });
    const accounts = await ctx.db.select().from(user);
    return c.json(directory.result.roles.map((r) => {
      const account = r.subject.kind === "principal" ? accounts.find((u) => u.corePrincipalId === r.subject.id) : undefined;
      const subject = r.subject.kind === "principal" ? directory.result.principals.find((p) => p.id === r.subject.id) : directory.result.groups.find((g) => g.id === r.subject.id);
      return { id: `${r.subject.kind}:${r.subject.id}:${r.role}`, userId: account?.id ?? r.subject.id,
        subject: r.subject, role: r.role, name: account?.name ?? subject?.displayName ?? r.subject.id,
        email: account?.email ?? (r.subject.kind === "group" ? "Group" : "Core principal"), createdAt: account?.createdAt ?? null };
    }));
  }));

  app.post("/:id/members", (c) => withGateway(c, async () => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access || access.role !== "admin") return c.json({ error: "universe admin required" }, 403);
    const body = await parseBody(c, memberAddSchema);
    if (!body.ok) return body.response;
    let subject: { kind: "principal" | "group"; id: string };
    if (body.data.groupId) subject = { kind: "group", id: body.data.groupId };
    else {
      const [target] = await ctx.db.select().from(user).where(body.data.userId ? eq(user.id, body.data.userId) : eq(user.email, body.data.email!)).limit(1);
      if (!target) return c.json({ error: "user not found" }, 404);
      subject = { kind: "principal", id: target.corePrincipalId };
    }
    const result = await deploymentClientFor(ctx).call("deployment/identity/apply", {
      operation: "assign_role", assignment: { scope: { kind: "universe", universeId: access.universe.lightspeedUniverseId }, subject, role: body.data.role },
    });
    return c.json(result.result, 201);
  }));

  app.patch("/:id/members/:memberId", (c) => withGateway(c, async () => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access || access.role !== "admin") return c.json({ error: "universe admin required" }, 403);
    const body = await parseBody(c, memberUpdateSchema);
    if (!body.ok) return body.response;
    const [kind, id, role, extra] = c.req.param("memberId").split(":");
    const parsed = z.object({ kind: z.enum(["principal", "group"]), id: z.string().uuid(), role: z.enum(["viewer", "contributor", "operator", "admin"]) }).safeParse({ kind, id, role });
    if (!parsed.success || extra !== undefined) return c.json({ error: "invalid assignment" }, 400);
    const result = await deploymentClientFor(ctx).call("deployment/identity/apply", {
      operation: "replace_role", assignment: { scope: { kind: "universe", universeId: access.universe.lightspeedUniverseId }, subject: { kind: parsed.data.kind, id: parsed.data.id }, role: parsed.data.role },
      role: body.data.role,
    });
    return c.json(result.result);
  }));

  app.delete("/:id/members/:memberId", (c) => withGateway(c, async () => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access || access.role !== "admin") return c.json({ error: "universe admin required" }, 403);
    const [kind, id, role] = c.req.param("memberId").split(":");
    const parsed = z.object({ kind: z.enum(["principal", "group"]), id: z.string().uuid(), role: z.enum(["viewer", "contributor", "operator", "admin"]) }).safeParse({ kind, id, role });
    if (!parsed.success) return c.json({ error: "invalid assignment" }, 400);
    const result = await deploymentClientFor(ctx).call("deployment/identity/apply", {
      operation: "revoke_role", assignment: { scope: { kind: "universe", universeId: access.universe.lightspeedUniverseId }, subject: { kind: parsed.data.kind, id: parsed.data.id }, role: parsed.data.role },
    });
    return c.json(result.result);
  }));

  return app;
}

export { universeForSession };
