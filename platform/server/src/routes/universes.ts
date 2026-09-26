import { Hono } from "hono";
import { z } from "zod";
import { and, eq, ne } from "drizzle-orm";
import { LightspeedRpcError, type MethodGroup } from "@lightspeed-ai/agent-client";
import { schema } from "@lightspeed/platform-db";
import {
  effectiveFeatures,
  memberAddSchema,
  memberUpdateSchema,
  mergeFeatureOverrides,
  slugify,
  universeCreateSchema,
  universeRoleSchema,
  universeUpdateSchema,
  type UniverseRole,
} from "@lightspeed/platform-shared";
import type { AppContext, ApiVariables } from "../context.js";
import { isPlatformAdmin } from "../context.js";
import { parseBody } from "../http.js";
import type { Member } from "../runtime-client.js";
import { deploymentClientFor, withGateway } from "./gateway.js";

const { universes, organization, member, user } = schema;

type UniverseRow = typeof universes.$inferSelect;

/// A universe as the API shows it: its row, where features are what is on
/// now (defaults and requirements applied), not the stored switches.
function universeView(row: UniverseRow, slug: string | null, role: UniverseRole | string | null) {
  return { ...row, features: effectiveFeatures(row.features), slug, role };
}

/// A universe the signed-in user may address, and as whom.
export interface UniverseAccess {
  universe: UniverseRow;
  slug: string;
  /// The member's role; a platform admin acts as `admin` in every universe.
  role: UniverseRole;
  member: Member;
}

/// Universe access for the current session: membership in the universe's
/// organization, or platform admin. `null` reads as not found. `slug` is the
/// organization slug, the universe's immutable URL segment.
async function universeForSession(
  ctx: AppContext,
  c: { get: (key: "session") => ApiVariables["session"] },
  universeId: string,
): Promise<UniverseAccess | null> {
  const [row] = await ctx.db
    .select({ universe: universes, slug: organization.slug })
    .from(universes)
    .innerJoin(organization, eq(organization.id, universes.organizationId))
    .where(eq(universes.id, universeId))
    .limit(1);
  if (!row) {
    return null;
  }
  const session = c.get("session");
  const [membership] = await ctx.db
    .select({ role: member.role })
    .from(member)
    .where(and(eq(member.organizationId, row.universe.organizationId), eq(member.userId, session.user.id)))
    .limit(1);
  const role = isPlatformAdmin(session) ? "admin" : universeRoleSchema.safeParse(membership?.role).data;
  if (!role) {
    return null;
  }
  return { universe: row.universe, slug: row.slug ?? "", role, member: { userId: session.user.id, role } };
}

/// Creates the platform half of a universe: an organization (slug probed to
/// a free one), the creator as its admin, and the universe row linked to the
/// given engine universe id. Shared by create (fresh engine id) and adopt
/// (existing engine id).
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
    await tx.insert(organization).values({ id: orgId, name, slug, createdAt: new Date() });
    await tx.insert(member).values({
      id: crypto.randomUUID(),
      organizationId: orgId,
      userId,
      role: "admin",
      createdAt: new Date(),
    });
    const [universe] = await tx
      .insert(universes)
      .values({ organizationId: orgId, lightspeedUniverseId, name })
      .returning();
    return universeView(universe!, slug, "admin");
  });
}

/// Whether removing or demoting `memberId` would leave the organization with
/// no admin.
async function isLastAdmin(ctx: AppContext, organizationId: string, memberId: string): Promise<boolean> {
  const [target] = await ctx.db
    .select({ role: member.role })
    .from(member)
    .where(and(eq(member.id, memberId), eq(member.organizationId, organizationId)))
    .limit(1);
  if (target?.role !== "admin") {
    return false;
  }
  const [other] = await ctx.db
    .select({ id: member.id })
    .from(member)
    .where(and(eq(member.organizationId, organizationId), eq(member.role, "admin"), ne(member.id, memberId)))
    .limit(1);
  return !other;
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
  /// Required, so a universe key never silently holds every group; core
  /// validates the names and refuses deployment groups.
  groups: z.array(z.string().min(1).max(100)).min(1).max(32),
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
        .leftJoin(member, and(eq(member.organizationId, universes.organizationId), eq(member.userId, session.user.id)));
      return c.json(rows.map((r) => universeView(r.universe, r.slug, r.role)));
    }
    const rows = await ctx.db
      .select({ universe: universes, slug: organization.slug, role: member.role })
      .from(universes)
      .innerJoin(organization, eq(organization.id, universes.organizationId))
      .innerJoin(member, eq(member.organizationId, universes.organizationId))
      .where(eq(member.userId, session.user.id));
    return c.json(rows.map((r) => universeView(r.universe, r.slug, r.role)));
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
    // The engine universe must exist before anything addresses it, so the
    // engine create comes first. Idempotent: if the platform transaction
    // below fails, the orphaned engine universe is empty, harmless, and
    // reaped by a deployment purge.
    const lightspeedUniverseId = crypto.randomUUID();
    return withGateway(c, async () => {
      await deploymentClientFor(ctx).call("deployment/universes/create", {
        universeId: lightspeedUniverseId,
      });
      const created = await createUniverseRows(ctx, session.user.id, input.name, baseSlug, lightspeedUniverseId);
      return c.json(created, 201);
    });
  });

  /// Reconciliation view (platform admin): every platform row checked
  /// against the default deployment's engine inventory, plus the engine
  /// universes no row links to (orphans — CLI/test tenants, or survivors
  /// of a purged platform DB). Rows with a custom gatewayUrl live on
  /// another deployment and are not checked here.
  app.get("/reconcile", async (c) => {
    if (!isPlatformAdmin(c.get("session"))) {
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
  /// half. The caller becomes its admin; engine data is untouched.
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
      await deploymentClientFor(ctx).call("deployment/universes/read", {
        universeId: input.lightspeedUniverseId,
      });
      const created = await createUniverseRows(ctx, session.user.id, input.name, slugify(input.name), input.lightspeedUniverseId);
      return c.json(created, 201);
    });
  });

  /// Repair for a phantom row (platform knows it, the engine does not —
  /// dev drift from backend swaps or engine resets): re-materializes the
  /// tenant. Safe because engine universes are born empty and create is
  /// idempotent; whatever data made it a phantom was already gone.
  app.post("/:id/engine", async (c) => {
    if (!isPlatformAdmin(c.get("session"))) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const access = await universeForSession(ctx, c, c.req.param("id"));
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

  /// Universe API keys, for a universe admin. Keys are minted with the
  /// Platform's deployment key, scoped to this universe, with the groups the
  /// admin chose and no actor assertion. The plaintext secret exists only in the
  /// create response and is never persisted by the platform.
  app.get("/:id/api-keys", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access || access.role !== "admin") {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const response = await deploymentClientFor(ctx, access.universe.gatewayUrl).call("deployment/api-keys/list", {
        scope: { kind: "universe", universeId: access.universe.lightspeedUniverseId },
      });
      return c.json(response.result.apiKeys ?? []);
    });
  });

  app.post("/:id/api-keys", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access || access.role !== "admin") {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, apiKeyCreateSchema);
    if (!body.ok) {
      return body.response;
    }
    return withGateway(c, async () => {
      const response = await deploymentClientFor(ctx, access.universe.gatewayUrl).call("deployment/api-keys/create", {
        scope: { kind: "universe", universeId: access.universe.lightspeedUniverseId },
        displayName: body.data.displayName,
        groups: body.data.groups as MethodGroup[],
        assertActor: false,
      });
      return c.json(response.result, 201);
    });
  });

  app.delete("/:id/api-keys/:keyPrefix", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access || access.role !== "admin") {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = deploymentClientFor(ctx, access.universe.gatewayUrl);
      // Revocation is by prefix across the deployment; only this universe's
      // keys are revocable from here.
      const keys = await client.call("deployment/api-keys/list", {
        scope: { kind: "universe", universeId: access.universe.lightspeedUniverseId },
      });
      const keyPrefix = c.req.param("keyPrefix");
      if (!(keys.result.apiKeys ?? []).some((key) => key.keyPrefix === keyPrefix)) {
        return c.json({ error: "not found" }, 404);
      }
      const response = await client.call("deployment/api-keys/revoke", { keyPrefix });
      return c.json(response.result.apiKey);
    });
  });

  /// Deletes an engine universe that has no platform row (orphan sweep —
  /// purges its sessions and blobs). Linked universes must go through
  /// archive → purge instead so platform state comes along.
  app.delete("/engine/:lightspeedUniverseId", async (c) => {
    if (!isPlatformAdmin(c.get("session"))) {
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
  /// rows (the organization cascades to membership, universe, setups). Gated
  /// to platform admins and archived universes — archive first, purge
  /// second, so no traffic races the purge.
  app.delete("/:id", async (c) => {
    if (!isPlatformAdmin(c.get("session"))) {
      return c.json({ error: "platform admin required" }, 403);
    }
    const access = await universeForSession(ctx, c, c.req.param("id"));
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
      await ctx.db.delete(organization).where(eq(organization.id, access.universe.organizationId));
      return c.json({ ok: true, purge });
    });
  });

  app.get("/:id", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return c.json(universeView(access.universe, access.slug, access.role));
  });

  app.patch("/:id", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    if (access.role !== "admin") {
      return c.json({ error: "universe admin required" }, 403);
    }
    const body = await parseBody(c, universeUpdateSchema);
    if (!body.ok) {
      return body.response;
    }
    // Display name lives on both rows (the organization is the sign-in face
    // of the universe); the slug never changes — links stay stable. Feature
    // switches merge into what the universe stored.
    const { features, ...fields } = body.data;
    const updated = await ctx.db.transaction(async (tx) => {
      if (fields.name) {
        await tx.update(organization).set({ name: fields.name }).where(eq(organization.id, access.universe.organizationId));
      }
      const [row] = await tx
        .update(universes)
        .set({
          ...fields,
          ...(features ? { features: mergeFeatureOverrides(access.universe.features, features) } : {}),
        })
        .where(eq(universes.id, access.universe.id))
        .returning();
      return row!;
    });
    return c.json(universeView(updated, access.slug, access.role));
  });

  /// Every member sees who else is in the universe and their role; emails
  /// are for admins.
  app.get("/:id/members", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
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
    return c.json(access.role === "admin" ? rows : rows.map(({ email: _email, ...row }) => row));
  });

  app.post("/:id/members", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    if (access.role !== "admin") {
      return c.json({ error: "universe admin required" }, 403);
    }
    const body = await parseBody(c, memberAddSchema);
    if (!body.ok) {
      return body.response;
    }
    const [target] = await ctx.db
      .select({ id: user.id })
      .from(user)
      .where(body.data.userId !== undefined ? eq(user.id, body.data.userId) : eq(user.email, body.data.email!))
      .limit(1);
    if (!target) {
      return c.json({ error: "user not found" }, 404);
    }
    const [existing] = await ctx.db
      .select({ id: member.id })
      .from(member)
      .where(and(eq(member.organizationId, access.universe.organizationId), eq(member.userId, target.id)))
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

  app.patch("/:id/members/:memberId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    if (access.role !== "admin") {
      return c.json({ error: "universe admin required" }, 403);
    }
    const body = await parseBody(c, memberUpdateSchema);
    if (!body.ok) {
      return body.response;
    }
    const memberId = c.req.param("memberId");
    if (body.data.role !== "admin" && (await isLastAdmin(ctx, access.universe.organizationId, memberId))) {
      return c.json({ error: "a universe keeps at least one admin" }, 409);
    }
    const [updated] = await ctx.db
      .update(member)
      .set({ role: body.data.role })
      .where(and(eq(member.id, memberId), eq(member.organizationId, access.universe.organizationId)))
      .returning();
    if (!updated) {
      return c.json({ error: "not found" }, 404);
    }
    return c.json(updated);
  });

  app.delete("/:id/members/:memberId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    if (access.role !== "admin") {
      return c.json({ error: "universe admin required" }, 403);
    }
    const memberId = c.req.param("memberId");
    if (await isLastAdmin(ctx, access.universe.organizationId, memberId)) {
      return c.json({ error: "a universe keeps at least one admin" }, 409);
    }
    const deleted = await ctx.db
      .delete(member)
      .where(and(eq(member.id, memberId), eq(member.organizationId, access.universe.organizationId)))
      .returning({ id: member.id });
    if (deleted.length === 0) {
      return c.json({ error: "not found" }, 404);
    }
    return c.json({ ok: true });
  });

  return app;
}

export { universeForSession };
