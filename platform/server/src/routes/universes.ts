import { Hono } from "hono";
import { z } from "zod";
import { and, eq } from "drizzle-orm";
import { LightspeedRpcError } from "@lightspeed/agent-client";
import { schema } from "@lightspeed/platform-db";
import {
  memberAddSchema,
  slugify,
  universeCreateSchema,
  universeUpdateSchema,
} from "@lightspeed/platform-shared";
import type { AppContext, ApiVariables } from "../context.js";
import { isPlatformAdmin } from "../context.js";
import { parseBody } from "../http.js";
import { operatorClientFor, withGateway } from "./gateway.js";

const { universes, organization, member, user } = schema;

type UniverseRow = typeof universes.$inferSelect;

/// Universe access for the current session: platform admins see everything;
/// otherwise membership in the universe's organization is required, and
/// mutations additionally require the owner/admin org role. `slug` is the
/// org slug — the universe's immutable URL segment.
async function universeForSession(
  ctx: AppContext,
  c: { get: (key: "session") => ApiVariables["session"] },
  universeId: string,
  write: boolean,
): Promise<{ universe: UniverseRow; slug: string; role: string } | null> {
  const [row] = await ctx.db
    .select({ universe: universes, slug: organization.slug })
    .from(universes)
    .innerJoin(organization, eq(organization.id, universes.organizationId))
    .where(eq(universes.id, universeId))
    .limit(1);
  if (!row) {
    return null;
  }
  const slug = row.slug ?? "";
  const session = c.get("session");
  if (isPlatformAdmin(session)) {
    return { universe: row.universe, slug, role: "platform-admin" };
  }
  const [membership] = await ctx.db
    .select()
    .from(member)
    .where(
      and(
        eq(member.organizationId, row.universe.organizationId),
        eq(member.userId, session.user.id),
      ),
    )
    .limit(1);
  if (!membership) {
    return null;
  }
  if (write && membership.role !== "owner" && membership.role !== "admin") {
    return null;
  }
  return { universe: row.universe, slug, role: membership.role };
}

/// Creates the platform half of a universe: a better-auth organization
/// (slug probed to a free one), owner membership for the caller, and the
/// universe row linked to the given engine universe id. Shared by create
/// (fresh engine id) and adopt (existing engine id).
async function createUniverseRows(
  ctx: AppContext,
  userId: string,
  name: string,
  baseSlug: string,
  lightspeedUniverseId: string,
) {
  return await ctx.db.transaction(async (tx) => {
    // Probe for a free slug (unique index is the backstop).
    let slug = baseSlug;
    for (let i = 2; ; i++) {
      const [existing] = await tx
        .select({ id: organization.id })
        .from(organization)
        .where(eq(organization.slug, slug))
        .limit(1);
      if (!existing) {
        break;
      }
      slug = `${baseSlug}-${i}`;
    }
    const orgId = crypto.randomUUID();
    await tx.insert(organization).values({
      id: orgId,
      name,
      slug,
      createdAt: new Date(),
    });
    await tx.insert(member).values({
      id: crypto.randomUUID(),
      organizationId: orgId,
      userId,
      role: "owner",
      createdAt: new Date(),
    });
    const [universe] = await tx
      .insert(universes)
      .values({
        organizationId: orgId,
        lightspeedUniverseId,
        name,
      })
      .returning();
    return { ...universe!, slug, role: "owner" };
  });
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
  displayName: z.string().trim().min(1).max(120),
});

export function universeRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  app.get("/", async (c) => {
    const session = c.get("session");
    if (isPlatformAdmin(session)) {
      // All universes; role is filled in where the admin happens to be a
      // member (the switcher shows memberships, the admin area shows all).
      const rows = await ctx.db
        .select({ universe: universes, slug: organization.slug, role: member.role })
        .from(universes)
        .innerJoin(organization, eq(organization.id, universes.organizationId))
        .leftJoin(
          member,
          and(
            eq(member.organizationId, universes.organizationId),
            eq(member.userId, session.user.id),
          ),
        );
      return c.json(rows.map((r) => ({ ...r.universe, slug: r.slug, role: r.role })));
    }
    const rows = await ctx.db
      .select({ universe: universes, slug: organization.slug, role: member.role })
      .from(universes)
      .innerJoin(organization, eq(organization.id, universes.organizationId))
      .innerJoin(member, eq(member.organizationId, universes.organizationId))
      .where(eq(member.userId, session.user.id));
    return c.json(rows.map((r) => ({ ...r.universe, slug: r.slug, role: r.role })));
  });

  app.post("/", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin(session)) {
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
    // or reaped by an operator purge.
    const lightspeedUniverseId = crypto.randomUUID();
    return withGateway(c, async () => {
      await operatorClientFor(ctx).call("operator/universes/create", {
        universeId: lightspeedUniverseId,
      });
      const created = await createUniverseRows(
        ctx,
        session.user.id,
        input.name,
        baseSlug,
        lightspeedUniverseId,
      );
      return c.json(created, 201);
    });
  });

  /// Reconciliation view (platform admin): every platform row checked
  /// against the default deployment's engine inventory, plus the engine
  /// universes no row links to (orphans — CLI/test tenants, or survivors
  /// of a purged platform DB). Rows with a custom gatewayUrl live on
  /// another deployment and are not checked here.
  app.get("/reconcile", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin(session)) {
      return c.json({ error: "platform admin required" }, 403);
    }
    return withGateway(c, async () => {
      const rows = await ctx.db.select().from(universes);
      const listed = await operatorClientFor(ctx).call("operator/universes/list", {});
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
  /// half. The caller becomes owner; engine data is untouched.
  app.post("/adopt", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin(session)) {
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
      await operatorClientFor(ctx).call("operator/universes/read", {
        universeId: input.lightspeedUniverseId,
      });
      const created = await createUniverseRows(
        ctx,
        session.user.id,
        input.name,
        slugify(input.name),
        input.lightspeedUniverseId,
      );
      return c.json(created, 201);
    });
  });

  /// Repair for a phantom row (platform knows it, the engine does not —
  /// dev drift from backend swaps or engine resets): re-materializes the
  /// tenant. Safe because engine universes are born empty and create is
  /// idempotent; whatever data made it a phantom was already gone.
  app.post("/:id/engine", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin(session)) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const created = await operatorClientFor(ctx, access.universe.gatewayUrl).call(
        "operator/universes/create",
        { universeId: access.universe.lightspeedUniverseId },
      );
      return c.json({ created: created.result.created });
    });
  });

  /// Universe API-key management stays operator-scoped engine-side: the platform
  /// authenticates the human, enforces owner/admin access, and supplies the
  /// engine universe id. The plaintext secret exists only in the create
  /// response and is never persisted by the platform.
  app.get("/:id/api-keys", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const response = await operatorClientFor(ctx, access.universe.gatewayUrl).call(
        "operator/api-keys/list",
        { universeId: access.universe.lightspeedUniverseId },
      );
      return c.json(response.result.apiKeys ?? []);
    });
  });

  app.post("/:id/api-keys", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, apiKeyCreateSchema);
    if (!body.ok) {
      return body.response;
    }
    const session = c.get("session");
    return withGateway(c, async () => {
      const response = await operatorClientFor(ctx, access.universe.gatewayUrl).call(
        "operator/api-keys/create",
        {
          universeId: access.universe.lightspeedUniverseId,
          displayName: body.data.displayName,
          principal: { kind: "user", id: session.user.id },
        },
      );
      return c.json(response.result, 201);
    });
  });

  app.delete("/:id/api-keys/:keyPrefix", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const response = await operatorClientFor(ctx, access.universe.gatewayUrl).call(
        "operator/api-keys/revoke",
        {
          universeId: access.universe.lightspeedUniverseId,
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
    if (!isPlatformAdmin(session)) {
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
      const report = await operatorClientFor(ctx).call("operator/universes/delete", {
        universeId: lightspeedUniverseId,
      });
      return c.json({ ok: true, purge: report.result });
    });
  });

  /// Permanent removal: engine purge (operator scope) then the platform
  /// rows (org cascade deletes membership, universe, bindings). Gated to
  /// platform admins and archived universes — archive first, purge second,
  /// so no traffic races the purge.
  app.delete("/:id", async (c) => {
    const session = c.get("session");
    if (!isPlatformAdmin(session)) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    if (access.universe.status !== "archived") {
      return c.json({ error: "archive the universe before deleting it" }, 409);
    }
    return withGateway(c, async () => {
      let purge: unknown = null;
      try {
        const report = await operatorClientFor(ctx, access.universe.gatewayUrl).call(
          "operator/universes/delete",
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
        .delete(organization)
        .where(eq(organization.id, access.universe.organizationId));
      return c.json({ ok: true, purge });
    });
  });

  app.get("/:id", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), false);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return c.json({ ...access.universe, slug: access.slug, role: access.role });
  });

  app.patch("/:id", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, universeUpdateSchema);
    if (!body.ok) {
      return body.response;
    }
    // Display name lives on both rows (org is the better-auth face of the
    // universe); the slug never changes — links stay stable.
    const updated = await ctx.db.transaction(async (tx) => {
      if (body.data.name) {
        await tx
          .update(organization)
          .set({ name: body.data.name })
          .where(eq(organization.id, access.universe.organizationId));
      }
      const [row] = await tx
        .update(universes)
        .set(body.data)
        .where(eq(universes.id, access.universe.id))
        .returning();
      return row;
    });
    return c.json({ ...updated, slug: access.slug, role: access.role });
  });

  app.get("/:id/members", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), false);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const rows = await ctx.db
      .select({
        id: member.id,
        userId: member.userId,
        role: member.role,
        email: user.email,
        name: user.name,
        createdAt: member.createdAt,
      })
      .from(member)
      .innerJoin(user, eq(user.id, member.userId))
      .where(eq(member.organizationId, access.universe.organizationId));
    return c.json(rows);
  });

  app.post("/:id/members", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, memberAddSchema);
    if (!body.ok) {
      return body.response;
    }
    const [target] = await ctx.db
      .select({ id: user.id })
      .from(user)
      .where(
        body.data.userId !== undefined
          ? eq(user.id, body.data.userId)
          : eq(user.email, body.data.email!),
      )
      .limit(1);
    if (!target) {
      return c.json({ error: "user not found" }, 404);
    }
    const [existing] = await ctx.db
      .select({ id: member.id })
      .from(member)
      .where(
        and(
          eq(member.organizationId, access.universe.organizationId),
          eq(member.userId, target.id),
        ),
      )
      .limit(1);
    if (existing) {
      return c.json({ error: "already a member" }, 409);
    }
    const [created] = await ctx.db
      .insert(member)
      .values({
        id: crypto.randomUUID(),
        organizationId: access.universe.organizationId,
        userId: target.id,
        role: body.data.role,
        createdAt: new Date(),
      })
      .returning();
    return c.json(created, 201);
  });

  app.delete("/:id/members/:memberId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const deleted = await ctx.db
      .delete(member)
      .where(
        and(
          eq(member.id, c.req.param("memberId")),
          eq(member.organizationId, access.universe.organizationId),
        ),
      )
      .returning({ id: member.id });
    if (deleted.length === 0) {
      return c.json({ error: "not found" }, 404);
    }
    return c.json({ ok: true });
  });

  return app;
}

export { universeForSession };
