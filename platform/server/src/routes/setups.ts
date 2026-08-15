import { and, eq, lt, ne, or } from "drizzle-orm";
import { Hono } from "hono";
import {
  LightspeedRpcError,
  type AgentProfileInput,
  type LightspeedClient,
  type McpServerInput,
} from "@lightspeed/agent-client";
import { schema } from "@lightspeed/platform-db";
import type { UniverseSetupState } from "@lightspeed/platform-db/schema";
import type { AppContext, ApiVariables } from "../context.js";
import { engineClientFor, operatorClientFor } from "./gateway.js";
import { universeForSession } from "./universes.js";

const SETUP_ID = "configurator";
const SETUP_VERSION = 3;
const SERVER_ID = "lsbot-configurator";
const PROFILE_ID = "lsbot-configurator";
const INSTALL_LEASE_MS = 5 * 60 * 1_000;

type Installation = typeof schema.universeSetupInstallations.$inferSelect;
type Universe = typeof schema.universes.$inferSelect;

export interface UniverseSetupView {
  id: string;
  name: string;
  description: string;
  version: number;
  available: boolean;
  status: "available" | "installing" | "ready" | "failed" | "unavailable";
  installedVersion?: number;
  error?: string;
  resources?: UniverseSetupState;
}

class SetupConflict extends Error {}
class SetupUnavailable extends Error {}

export function setupRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  app.get("/:id/setups", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const installation = await findInstallation(ctx, access.universe.id);
    return c.json([setupView(ctx, installation)]);
  });

  app.post("/:id/setups/configurator/install", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const session = c.get("session");
    try {
      const installation = await claimInstallation(ctx, access.universe.id, session.user.id);
      try {
        const completed = await installConfigurator(
          ctx,
          access.universe,
          session.user.id,
          installation,
        );
        return c.json(setupView(ctx, completed));
      } catch (error) {
        await markFailed(ctx, installation.id, safeErrorMessage(error));
        throw error;
      }
    } catch (error) {
      if (error instanceof SetupConflict) {
        return c.json({ error: error.message }, 409);
      }
      if (error instanceof SetupUnavailable) {
        return c.json({ error: error.message }, 501);
      }
      if (error instanceof LightspeedRpcError) {
        const status = error.kind === "conflict" ? 409 : error.kind === "not_found" ? 404 : 502;
        return c.json({ error: `engine error: ${error.message}` }, status);
      }
      return c.json({ error: safeErrorMessage(error) }, 502);
    }
  });

  return app;
}

function setupView(ctx: AppContext, installation: Installation | null): UniverseSetupView {
  const available = ctx.env.configuratorMcpUrl !== null;
  return {
    id: SETUP_ID,
    name: "Configurator",
    description:
      "Creates a dedicated credential, registers the Configurator MCP server, and adds a ready-to-use profile for managing this universe.",
    version: SETUP_VERSION,
    available,
    status: installation
      ? installation.status
      : available
        ? "available"
        : "unavailable",
    ...(installation ? { installedVersion: installation.installedVersion } : {}),
    ...(installation?.error ? { error: installation.error } : {}),
    ...(installation ? { resources: installation.state } : {}),
  };
}

async function findInstallation(ctx: AppContext, universeId: string): Promise<Installation | null> {
  const [installation] = await ctx.db
    .select()
    .from(schema.universeSetupInstallations)
    .where(
      and(
        eq(schema.universeSetupInstallations.universeId, universeId),
        eq(schema.universeSetupInstallations.setupId, SETUP_ID),
      ),
    )
    .limit(1);
  return installation ?? null;
}

async function claimInstallation(
  ctx: AppContext,
  universeId: string,
  userId: string,
): Promise<Installation> {
  if (!ctx.env.configuratorMcpUrl) {
    throw new SetupUnavailable("Configurator MCP URL is not configured");
  }
  const now = new Date();
  const [inserted] = await ctx.db
    .insert(schema.universeSetupInstallations)
    .values({
      universeId,
      setupId: SETUP_ID,
      status: "installing",
      installedByUserId: userId,
      updatedAt: now,
    })
    .onConflictDoNothing()
    .returning();
  if (inserted) {
    return inserted;
  }

  const staleBefore = new Date(now.getTime() - INSTALL_LEASE_MS);
  const [claimed] = await ctx.db
    .update(schema.universeSetupInstallations)
    .set({
      status: "installing",
      error: null,
      installedByUserId: userId,
      updatedAt: now,
    })
    .where(
      and(
        eq(schema.universeSetupInstallations.universeId, universeId),
        eq(schema.universeSetupInstallations.setupId, SETUP_ID),
        or(
          ne(schema.universeSetupInstallations.status, "installing"),
          lt(schema.universeSetupInstallations.updatedAt, staleBefore),
        ),
      ),
    )
    .returning();
  if (!claimed) {
    throw new SetupConflict("Configurator setup installation is already running");
  }
  return claimed;
}

async function installConfigurator(
  ctx: AppContext,
  universe: Universe,
  userId: string,
  installation: Installation,
): Promise<Installation> {
  const mcpUrl = ctx.env.configuratorMcpUrl;
  if (!mcpUrl) {
    throw new SetupUnavailable("Configurator MCP URL is not configured");
  }
  new URL(mcpUrl);

  const principal = `user:${userId}`;
  const client = engineClientFor(ctx, universe, principal);
  const operator = operatorClientFor(ctx, universe.gatewayUrl);
  let state = { ...installation.state };

  state = await ensureCredential(
    ctx,
    installation.id,
    universe,
    client,
    operator,
    state,
    userId,
    mcpUrl,
  );
  state = await ensureMcpServer(ctx, installation.id, client, state, mcpUrl);
  state = await ensureProfile(ctx, installation.id, client, state);

  const [completed] = await ctx.db
    .update(schema.universeSetupInstallations)
    .set({
      installedVersion: SETUP_VERSION,
      status: "ready",
      state,
      error: null,
      updatedAt: new Date(),
    })
    .where(eq(schema.universeSetupInstallations.id, installation.id))
    .returning();
  if (!completed) {
    throw new Error("setup installation row disappeared during install");
  }
  return completed;
}

async function ensureCredential(
  ctx: AppContext,
  installationId: string,
  universe: Universe,
  client: LightspeedClient,
  operator: LightspeedClient,
  state: UniverseSetupState,
  userId: string,
  mcpUrl: string,
): Promise<UniverseSetupState> {
  const keys = await operator.call("operator/api-keys/list", {
    universeId: universe.lightspeedUniverseId,
  });
  const key = state.keyPrefix
    ? (keys.result.apiKeys ?? []).find((candidate) => candidate.keyPrefix === state.keyPrefix)
    : undefined;
  const grant = state.grantId
    ? await readGrant(client, state.grantId)
    : null;
  if (
    key &&
    key.revokedAtMs == null &&
    grant?.status === "active" &&
    grant.audience === mcpUrl
  ) {
    return state;
  }

  if (grant?.status === "active" && state.grantId) {
    await client.call("auth/grants/revoke", { grantId: state.grantId });
  }
  if (key && key.revokedAtMs == null && state.keyPrefix) {
    await operator.call("operator/api-keys/revoke", {
      universeId: universe.lightspeedUniverseId,
      keyPrefix: state.keyPrefix,
    });
  }

  const minted = await operator.call("operator/api-keys/create", {
    universeId: universe.lightspeedUniverseId,
    displayName: "Lightspeed Configurator setup",
    principal: { kind: "user", id: userId },
  });
  const grantId = `authgrant_lsbot_configurator_${crypto.randomUUID().replaceAll("-", "")}`;
  try {
    await client.call("auth/grants/import", {
      grantId,
      providerId: "lsbot-configurator",
      token: minted.result.secret,
      displayName: "Lightspeed Configurator setup",
      audience: mcpUrl,
    });
  } catch (error) {
    await operator
      .call("operator/api-keys/revoke", {
        universeId: universe.lightspeedUniverseId,
        keyPrefix: minted.result.apiKey.keyPrefix,
      })
      .catch(() => undefined);
    throw error;
  }

  return await persistState(ctx, installationId, {
    ...state,
    keyPrefix: minted.result.apiKey.keyPrefix,
    grantId,
  });
}

async function ensureMcpServer(
  ctx: AppContext,
  installationId: string,
  client: LightspeedClient,
  state: UniverseSetupState,
  mcpUrl: string,
): Promise<UniverseSetupState> {
  if (!state.grantId) {
    throw new Error("Configurator auth grant was not created");
  }
  const existing = await readMcpServer(client, SERVER_ID);
  if (existing && state.serverId !== SERVER_ID) {
    throw new SetupConflict(`MCP server id ${SERVER_ID} already exists and is not setup-managed`);
  }
  const server: McpServerInput = {
    serverId: SERVER_ID,
    displayName: "Lightspeed Configurator",
    serverUrl: mcpUrl,
    transport: "streamableHttp",
    defaultServerLabel: "configurator",
    description: "Configure and operate this Lightspeed universe through its generated API.",
    approvalDefault: "never",
    authPolicy: { type: "requiredBearer" },
    credential: { type: "authGrant", grantId: state.grantId },
    status: "active",
  };
  await client.call("mcp/servers/put", {
    server,
    ...(existing ? { expectedRevision: existing.revision } : {}),
  });
  return await persistState(ctx, installationId, { ...state, serverId: SERVER_ID });
}

async function ensureProfile(
  ctx: AppContext,
  installationId: string,
  client: LightspeedClient,
  state: UniverseSetupState,
): Promise<UniverseSetupState> {
  const existing = await readProfile(client, PROFILE_ID);
  if (existing && state.profileId !== PROFILE_ID) {
    throw new SetupConflict(`profile id ${PROFILE_ID} already exists and is not setup-managed`);
  }
  const profile: AgentProfileInput = {
    profileId: PROFILE_ID,
    displayName: "Universe Configurator",
    description: "A ready-to-use agent profile for configuring and operating this universe.",
    instructions: {
      type: "text",
      text:
        "Configure and operate the current Lightspeed universe. Read revisioned resources before replacing them, make only the requested changes, and report the resulting resource identifiers.",
    },
    config: {
      features: {
        mcp: {
          servers: [
            {
              serverId: SERVER_ID,
              approval: "never",
            },
          ],
        },
      },
    },
  };
  await client.call("profiles/put", {
    profile,
    ...(existing ? { expectedRevision: existing.revision } : {}),
  });
  return await persistState(ctx, installationId, { ...state, profileId: PROFILE_ID });
}

async function readGrant(client: LightspeedClient, grantId: string) {
  try {
    return (await client.call("auth/grants/read", { grantId })).result.grant;
  } catch (error) {
    if (isNotFound(error)) return null;
    throw error;
  }
}

async function readMcpServer(client: LightspeedClient, serverId: string) {
  try {
    return (await client.call("mcp/servers/read", { serverId })).result.server;
  } catch (error) {
    if (isNotFound(error)) return null;
    throw error;
  }
}

async function readProfile(client: LightspeedClient, profileId: string) {
  try {
    return (await client.call("profiles/read", { profileId })).result.profile;
  } catch (error) {
    if (isNotFound(error)) return null;
    throw error;
  }
}

function isNotFound(error: unknown): boolean {
  return error instanceof LightspeedRpcError && error.kind === "not_found";
}

async function persistState(
  ctx: AppContext,
  installationId: string,
  state: UniverseSetupState,
): Promise<UniverseSetupState> {
  await ctx.db
    .update(schema.universeSetupInstallations)
    .set({ state, updatedAt: new Date() })
    .where(eq(schema.universeSetupInstallations.id, installationId));
  return state;
}

async function markFailed(ctx: AppContext, installationId: string, error: string) {
  await ctx.db
    .update(schema.universeSetupInstallations)
    .set({ status: "failed", error, updatedAt: new Date() })
    .where(eq(schema.universeSetupInstallations.id, installationId));
}

function safeErrorMessage(error: unknown): string {
  if (error instanceof SetupConflict || error instanceof SetupUnavailable) {
    return error.message;
  }
  if (error instanceof LightspeedRpcError) {
    return `engine error: ${error.message}`;
  }
  return error instanceof Error ? error.message : String(error);
}
