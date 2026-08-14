import { Hono } from "hono";
import { desc, eq } from "drizzle-orm";
import { z } from "zod";
import { LightspeedRpcError } from "@lightspeed/agent-client";
import { schema } from "@lightspeed/platform-db";
import { Client, Connection } from "@temporalio/client";
import {
  FOUNDRY_CONFIG_SIGNAL,
  FOUNDRY_DEFAULT_PROFILE_ID,
  FOUNDRY_EVENT_SIGNAL,
  FOUNDRY_PACK_WORKFLOW,
  FOUNDRY_STATE_QUERY,
  FOUNDRY_WORKFLOW_TASK_QUEUE,
  foundryDefaultProfile,
  foundryPackWorkflowId,
  type FoundryEvent,
  type FoundryEventDocumentV1,
  type FoundryPackStartV1,
} from "@lightspeed/foundry/contracts";
import type { FoundryPackSnapshot } from "@lightspeed/foundry/workflows";
import type { AppContext, ApiVariables } from "../context.js";
import { parseBody } from "../http.js";
import { engineClientFor } from "./gateway.js";
import { universeForSession } from "./universes.js";

const { foundryPacks, foundryReleases } = schema;

const packName = z
  .string()
  .regex(/^[a-z0-9][a-z0-9-]*$/, "lowercase alphanumerics and dashes")
  .max(64);
const runtimeTargetSchema = z.object({
  kind: z.enum(["docker", "kubernetes", "ssh", "external"]),
  name: z.string().trim().min(1).max(200),
  metadata: z.record(z.string(), z.string()).optional(),
});
const packCreateSchema = z.object({
  name: packName,
  repoUrl: z.string().url(),
  kind: z.literal("workflow").default("workflow"),
  managerProfileId: z.string().trim().min(1).default(FOUNDRY_DEFAULT_PROFILE_ID),
  environmentId: z.string().trim().min(1).nullish(),
  runtimeTarget: runtimeTargetSchema.nullish(),
});
const packUpdateSchema = z
  .object({
    repoUrl: z.string().url().optional(),
    managerProfileId: z.string().trim().min(1).optional(),
    environmentId: z.string().trim().min(1).nullable().optional(),
    runtimeTarget: runtimeTargetSchema.nullable().optional(),
    enabled: z.boolean().optional(),
  })
  .refine((value) => Object.keys(value).length > 0, "at least one field is required");
const eventCreateSchema = z.object({
  id: z.string().trim().min(1).max(200).optional(),
  kind: z.string().trim().min(1).max(200),
  source: z.string().trim().min(1).max(200).default("manual"),
  occurredAt: z.string().datetime({ offset: true }).optional(),
  summary: z.string().trim().min(1).max(2_000),
  data: z.unknown().optional(),
  correlationId: z.string().trim().min(1).max(200).nullable().optional(),
  causationId: z.string().trim().min(1).max(200).nullable().optional(),
  links: z.array(z.string().url()).max(20).optional(),
  rawRef: z.string().regex(/^sha256:[0-9a-f]{64}$/).nullable().optional(),
});

let temporalClient: Promise<Client> | null = null;
function getTemporal(): Promise<Client> {
  temporalClient ??= Connection.connect({
    address: process.env.TEMPORAL_ADDRESS ?? "localhost:7233",
  }).then(
    (connection) =>
      new Client({
        connection,
        namespace: process.env.TEMPORAL_NAMESPACE ?? "default",
      }),
  );
  return temporalClient;
}

export function foundryRoutes(ctx: AppContext) {
  const byUniverse = new Hono<{ Variables: ApiVariables }>();
  const packs = new Hono<{ Variables: ApiVariables }>();

  byUniverse.get("/:id/foundry-packs", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), false);
    if (!access) return c.json({ error: "not found" }, 404);
    const rows = await ctx.db
      .select()
      .from(foundryPacks)
      .where(eq(foundryPacks.universeId, access.universe.id))
      .orderBy(foundryPacks.name);
    return c.json({ packs: rows });
  });

  byUniverse.post("/:id/foundry-packs", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) return c.json({ error: "not found" }, 404);
    const body = await parseBody(c, packCreateSchema);
    if (!body.ok) return body.response;

    const engine = engineClientFor(ctx, access.universe);
    if (body.data.managerProfileId === FOUNDRY_DEFAULT_PROFILE_ID) {
      await ensureDefaultFoundryProfile(engine);
    } else {
      await engine.call("profiles/read", { profileId: body.data.managerProfileId });
    }

    const [pack] = await ctx.db
      .insert(foundryPacks)
      .values({
        universeId: access.universe.id,
        name: body.data.name,
        kind: body.data.kind,
        repoUrl: body.data.repoUrl,
        managerProfileId: body.data.managerProfileId,
        environmentId: body.data.environmentId ?? null,
        runtimeTarget: body.data.runtimeTarget ?? null,
      })
      .onConflictDoNothing()
      .returning();
    if (!pack) return c.json({ error: "a pack with that name already exists" }, 409);

    try {
      await signalConfig(packStart(pack, access.universe.lightspeedUniverseId));
    } catch (error) {
      await ctx.db.delete(foundryPacks).where(eq(foundryPacks.id, pack.id));
      return c.json(
        { error: "failed to start the Foundry controller", failure: errorMessage(error) },
        502,
      );
    }
    return c.json({ pack }, 201);
  });

  async function packForSession(
    c: Parameters<typeof universeForSession>[1],
    packIdParam: string,
    write: boolean,
  ) {
    const [pack] = await ctx.db
      .select()
      .from(foundryPacks)
      .where(eq(foundryPacks.id, packIdParam))
      .limit(1);
    if (!pack) return null;
    const access = await universeForSession(ctx, c, pack.universeId, write);
    return access ? { pack, access } : null;
  }

  packs.get("/:id", async (c) => {
    const found = await packForSession(c, c.req.param("id"), false);
    if (!found) return c.json({ error: "not found" }, 404);
    return c.json({ pack: found.pack });
  });

  packs.patch("/:id", async (c) => {
    const found = await packForSession(c, c.req.param("id"), true);
    if (!found) return c.json({ error: "not found" }, 404);
    const body = await parseBody(c, packUpdateSchema);
    if (!body.ok) return body.response;
    if (body.data.managerProfileId !== undefined) {
      await engineClientFor(ctx, found.access.universe).call("profiles/read", {
        profileId: body.data.managerProfileId,
      });
    }
    const [pack] = await ctx.db
      .update(foundryPacks)
      .set(body.data)
      .where(eq(foundryPacks.id, found.pack.id))
      .returning();
    if (!pack) return c.json({ error: "not found" }, 404);
    try {
      await signalConfig(packStart(pack, found.access.universe.lightspeedUniverseId));
    } catch (error) {
      await ctx.db
        .update(foundryPacks)
        .set({
          repoUrl: found.pack.repoUrl,
          managerProfileId: found.pack.managerProfileId,
          environmentId: found.pack.environmentId,
          runtimeTarget: found.pack.runtimeTarget,
          enabled: found.pack.enabled,
        })
        .where(eq(foundryPacks.id, found.pack.id));
      return c.json(
        { error: "controller reconciliation failed; configuration was not changed", failure: errorMessage(error) },
        502,
      );
    }
    return c.json({ pack });
  });

  packs.get("/:id/state", async (c) => {
    const found = await packForSession(c, c.req.param("id"), false);
    if (!found) return c.json({ error: "not found" }, 404);
    const temporal = await getTemporal();
    const handle = temporal.workflow.getHandle(
      foundryPackWorkflowId(found.access.universe.lightspeedUniverseId, found.pack.name),
    );
    try {
      const state = await handle.query<FoundryPackSnapshot>(FOUNDRY_STATE_QUERY);
      return c.json({ state });
    } catch (error) {
      return c.json({ error: "Foundry controller unavailable", failure: errorMessage(error) }, 503);
    }
  });

  packs.post("/:id/events", async (c) => {
    const found = await packForSession(c, c.req.param("id"), true);
    if (!found) return c.json({ error: "not found" }, 404);
    if (!found.pack.enabled) return c.json({ error: "pack is disabled" }, 409);
    const body = await parseBody(c, eventCreateSchema);
    if (!body.ok) return body.response;
    const document: FoundryEventDocumentV1 = {
      version: 1,
      kind: body.data.kind,
      source: body.data.source,
      occurredAt: body.data.occurredAt ?? new Date().toISOString(),
      summary: body.data.summary,
      ...(body.data.data === undefined ? {} : { data: body.data.data }),
      ...(body.data.correlationId === undefined ? {} : { correlationId: body.data.correlationId }),
      ...(body.data.causationId === undefined ? {} : { causationId: body.data.causationId }),
      ...(body.data.links === undefined ? {} : { links: body.data.links }),
      ...(body.data.rawRef === undefined ? {} : { rawRef: body.data.rawRef }),
    };
    const engine = engineClientFor(ctx, found.access.universe);
    const stored = await engine.call("blobs/put", {
      blobs: [
        {
          bytesBase64: Buffer.from(JSON.stringify(document), "utf8").toString("base64"),
        },
      ],
    });
    const ref = stored.result.blobs?.[0]?.blobRef;
    if (!ref) return c.json({ error: "event document storage returned no ref" }, 502);
    const event: FoundryEvent = { version: 1, id: body.data.id ?? crypto.randomUUID(), ref };
    const config = packStart(found.pack, found.access.universe.lightspeedUniverseId);
    const temporal = await getTemporal();
    await temporal.workflow.signalWithStart(FOUNDRY_PACK_WORKFLOW, {
      workflowId: foundryPackWorkflowId(config.universeId, config.packName),
      taskQueue: FOUNDRY_WORKFLOW_TASK_QUEUE,
      args: [config],
      signal: FOUNDRY_EVENT_SIGNAL,
      signalArgs: [event],
    });
    return c.json({ event, document }, 202);
  });

  packs.get("/:id/releases", async (c) => {
    const found = await packForSession(c, c.req.param("id"), false);
    if (!found) return c.json({ error: "not found" }, 404);
    const releases = await ctx.db
      .select()
      .from(foundryReleases)
      .where(eq(foundryReleases.packId, found.pack.id))
      .orderBy(desc(foundryReleases.createdAt))
      .limit(50);
    return c.json({ releases });
  });

  return { byUniverse, packs };
}

type PackRow = typeof foundryPacks.$inferSelect;

function packStart(pack: PackRow, universeId: string): FoundryPackStartV1 {
  return {
    version: 1,
    universeId,
    packId: pack.id,
    packName: pack.name,
    repoUrl: pack.repoUrl,
    managerProfileId: pack.managerProfileId,
    environmentId: pack.environmentId,
    runtimeTarget: pack.runtimeTarget,
    enabled: pack.enabled,
  };
}

async function signalConfig(config: FoundryPackStartV1): Promise<void> {
  const temporal = await getTemporal();
  await temporal.workflow.signalWithStart(FOUNDRY_PACK_WORKFLOW, {
    workflowId: foundryPackWorkflowId(config.universeId, config.packName),
    taskQueue: FOUNDRY_WORKFLOW_TASK_QUEUE,
    args: [config],
    signal: FOUNDRY_CONFIG_SIGNAL,
    signalArgs: [config],
  });
}

async function ensureDefaultFoundryProfile(
  client: ReturnType<typeof engineClientFor>,
): Promise<void> {
  try {
    await client.call("profiles/read", { profileId: FOUNDRY_DEFAULT_PROFILE_ID });
  } catch (error) {
    if (!(error instanceof LightspeedRpcError) || error.kind !== "not_found") throw error;
    await client.call("profiles/put", { profile: foundryDefaultProfile() });
  }
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
