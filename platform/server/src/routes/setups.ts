import { and, eq, lt, ne, or } from "drizzle-orm";
import { Hono } from "hono";
import {
  LightspeedRpcError,
  type AgentProfileInput,
  type LightspeedClient,
  type McpServerInput,
  type MethodGroup,
} from "@lightspeed-ai/agent-client";
import { z } from "zod";
import { schema } from "@lightspeed/platform-db";
import type { UniverseSetupState } from "@lightspeed/platform-db/schema";
import type { AppContext, ApiVariables } from "../context.js";
import { parseBody } from "../http.js";
import { universeKeyClient } from "../runtime-client.js";
import { deploymentClientFor, engineClientFor } from "./gateway.js";
import { universeForSession, type UniverseAccess } from "./universes.js";

const SETUP_ID = "configurator";
const SETUP_VERSION = 5;
const SERVER_ID = "lightspeed-configurator";
const PROFILE_ID = "lightspeed-configurator";
const KEY_DISPLAY_NAME = "Lightspeed Configurator service credential";
const INSTALL_LEASE_MS = 5 * 60 * 1_000;
const CONFIGURATOR_DESCRIPTION =
  "Creates a dedicated credential, registers the Configurator MCP server, and adds a ready-to-use profile for managing this universe. " +
  "The server offers only the tools its key may call, and anyone who can attach it acts with that key.";

/// Which key the Configurator acts with, chosen on every install, repair and
/// upgrade: a new key the setup mints and owns, an existing universe key
/// whose secret the admin pastes (core keeps only hashes, so it cannot be
/// read back), or the key the installation already uses.
const keyChoiceSchema = z.discriminatedUnion("kind", [
  z.object({
    kind: z.literal("new"),
    /// Core validates the names and refuses deployment groups.
    groups: z.array(z.string().min(1).max(100)).min(1).max(32),
  }),
  z.object({
    kind: z.literal("existing"),
    keyPrefix: z.string().min(1).max(64),
    secret: z.string().regex(/^lsk_\S+$/).max(512),
  }),
  z.object({ kind: z.literal("current") }),
]);
const installSchema = z.object({ key: keyChoiceSchema });
export type KeyChoice = z.infer<typeof keyChoiceSchema>;

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
class SetupInvalid extends Error {}
class SetupUnavailable extends Error {}

export function setupRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  /// Operators see what templates exist; installing one stays with Admins
  /// because it mints a key that configures the universe.
  app.get("/:id/setups", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access || (access.role !== "admin" && access.role !== "operator")) {
      return c.json({ error: "not found" }, 404);
    }
    const installation = await findInstallation(ctx, access.universe.id);
    return c.json([setupView(ctx, installation)]);
  });

  app.post("/:id/setups/configurator/install", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"));
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    if (access.role !== "admin") {
      return c.json({ error: "universe admin required" }, 403);
    }
    const body = await parseBody(c, installSchema);
    if (!body.ok) {
      return body.response;
    }
    const key = body.data.key;
    const session = c.get("session");
    try {
      const installation = await claimInstallation(ctx, access.universe.id, session.user.id);
      try {
        const completed = await installConfigurator(
          ctx,
          access,
          installation,
          key,
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
      if (error instanceof SetupInvalid) {
        return c.json({ error: error.message }, 400);
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
    description: CONFIGURATOR_DESCRIPTION,
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
  access: UniverseAccess,
  installation: Installation,
  key: KeyChoice,
): Promise<Installation> {
  const mcpUrl = ctx.env.configuratorMcpUrl;
  if (!mcpUrl) {
    throw new SetupUnavailable("Configurator MCP URL is not configured");
  }
  new URL(mcpUrl);

  const universe = access.universe;
  const client = engineClientFor(ctx, access);
  const deployment = deploymentClientFor(ctx, universe.gatewayUrl);
  let state = { ...installation.state };

  const verify = (secret: string) => universeKeyClient(ctx.env, universe.gatewayUrl, secret);
  state = await ensureCredential(ctx, installation.id, universe, client, deployment, verify, state, mcpUrl, key);
  state = await ensureMcpServer(
    ctx,
    installation.id,
    client,
    state,
    mcpUrl,
    ctx.env.configuratorMcpAllowPrivateNetwork,
  );
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

/// Points the setup's bearer grant at the chosen key. A key the setup
/// minted is revoked once replaced; a key an admin brought is never revoked
/// here, only the grant holding its secret. Retrying the same choice keeps
/// a live key and grant.
export async function ensureCredential(
  ctx: AppContext,
  installationId: string,
  universe: Pick<Universe, "lightspeedUniverseId">,
  client: LightspeedClient,
  deployment: LightspeedClient,
  verify: (secret: string) => LightspeedClient,
  state: UniverseSetupState,
  mcpUrl: string,
  choice: KeyChoice,
): Promise<UniverseSetupState> {
  const keys = (await deployment.call("deployment/api-keys/list", {
    scope: { kind: "universe", universeId: universe.lightspeedUniverseId },
  })).result.apiKeys ?? [];
  const live = (prefix: string | undefined) =>
    prefix ? keys.find((key) => key.keyPrefix === prefix && key.revokedAtMs == null) : undefined;
  const current = live(state.keyPrefix);
  const grant = state.grantId ? await readGrant(client, state.grantId) : null;
  const usable = current !== undefined && grant?.status === "active" && grant.audience === mcpUrl;
  const minted = state.keySource !== "existing";

  /// Revokes the grant, and the key too when the setup minted it and it is
  /// not the key being kept.
  const retire = async (keeping?: string) => {
    if (grant?.status === "active" && state.grantId) {
      await client.call("auth/grants/revoke", { grantId: state.grantId });
    }
    if (current && minted && current.keyPrefix !== keeping) {
      await deployment.call("deployment/api-keys/revoke", { keyPrefix: current.keyPrefix });
    }
  };

  switch (choice.kind) {
    case "current": {
      if (!usable) {
        throw new SetupConflict("The Configurator's key or credential is no longer active; choose a key");
      }
      return await persistState(ctx, installationId, { ...state, keyGroups: current.groups });
    }
    case "new": {
      const groups = [...new Set(choice.groups)].sort() as MethodGroup[];
      if (usable && minted && current.displayName === KEY_DISPLAY_NAME && sameGroups(current.groups, groups)) {
        return await persistState(ctx, installationId, { ...state, keyGroups: current.groups });
      }
      await retire();
      const created = await deployment.call("deployment/api-keys/create", {
        scope: { kind: "universe", universeId: universe.lightspeedUniverseId },
        displayName: KEY_DISPLAY_NAME,
        groups,
        assertActor: false,
      });
      const grantId = await importGrant(client, created.result.secret, mcpUrl).catch(async (error: unknown) => {
        await deployment
          .call("deployment/api-keys/revoke", { keyPrefix: created.result.apiKey.keyPrefix })
          .catch(() => undefined);
        throw error;
      });
      return await persistState(ctx, installationId, {
        ...state,
        keyPrefix: created.result.apiKey.keyPrefix,
        keyGroups: groups,
        keySource: "minted",
        grantId,
      });
    }
    case "existing": {
      const chosen = live(choice.keyPrefix);
      if (!chosen) {
        throw new SetupInvalid("That key is not an active key of this universe");
      }
      if (usable && current.keyPrefix === chosen.keyPrefix) {
        return await persistState(ctx, installationId, { ...state, keyGroups: chosen.groups });
      }
      // Core names the key a secret belongs to; a wrong or revoked secret
      // never reaches the credential store.
      const caller = await verify(choice.secret)
        .call("initialize", { clientInfo: { name: "lightspeed-platform", version: null }, capabilities: null })
        .then((response) => response.result.caller, () => null);
      if (caller?.keyPrefix !== chosen.keyPrefix) {
        throw new SetupInvalid("The secret does not belong to the chosen key");
      }
      await retire(chosen.keyPrefix);
      const grantId = await importGrant(client, choice.secret, mcpUrl);
      return await persistState(ctx, installationId, {
        ...state,
        keyPrefix: chosen.keyPrefix,
        keyGroups: chosen.groups,
        keySource: "existing",
        grantId,
      });
    }
  }
}

async function importGrant(client: LightspeedClient, token: string, mcpUrl: string): Promise<string> {
  const grantId = `authgrant_lightspeed_configurator_${crypto.randomUUID().replaceAll("-", "")}`;
  await client.call("auth/grants/import", {
    grantId,
    providerId: "lightspeed-configurator",
    token,
    displayName: "Lightspeed Configurator setup",
    audience: mcpUrl,
  });
  return grantId;
}

function sameGroups(left: readonly string[], right: readonly string[]): boolean {
  const set = new Set(left);
  return set.size === new Set(right).size && right.every((group) => set.has(group));
}

export async function ensureMcpServer(
  ctx: AppContext,
  installationId: string,
  client: LightspeedClient,
  state: UniverseSetupState,
  mcpUrl: string,
  allowPrivateNetwork: boolean,
): Promise<UniverseSetupState> {
  if (!state.grantId) {
    throw new Error("Configurator auth grant was not created");
  }
  const existing = await readMcpServer(client, SERVER_ID);
  if (existing && state.serverId !== SERVER_ID) {
    throw new SetupConflict(`MCP server id ${SERVER_ID} already exists and is not setup-managed`);
  }
  const auth: Pick<McpServerInput, "authPolicy" | "credential"> = {
    authPolicy: { type: "requiredBearer" },
    credential: { type: "authGrant", grantId: state.grantId },
  };
  const server: McpServerInput = {
    serverId: SERVER_ID,
    displayName: "Lightspeed Configurator",
    serverUrl: mcpUrl,
    defaultServerLabel: "configurator",
    description: "Configure and operate this Lightspeed universe through its generated API.",
    execution: "native",
    exposure: "search",
    approval: "never",
    allowPrivateNetwork,
    ...auth,
    status: "active",
  };
  // The server acts with the Configurator key, so whoever may attach it
  // configures the universe with that key's groups.
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
