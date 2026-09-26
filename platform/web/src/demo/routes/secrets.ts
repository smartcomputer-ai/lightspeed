/// Universe secrets and what is built on them: auth grants, model provider
/// keys, coding-agent subscriptions, GitHub Apps, model discovery, and setup
/// installation. Secret values are accepted and dropped; only the non-secret
/// views the real gateway returns are kept.
import { Hono } from "hono";
import type {
  GitHubApp,
  GitHubInstallation,
  McpServer,
  ModelEndpointConfig,
  ModelListResponse,
  ModelProviderDiscovery,
  ProfileDocument,
  SecretGrant,
  SecretProvider,
  UniverseSetup,
} from "@/api";
import { base64ToText, type DemoStore, type UniverseState } from "../store";
import { badRequest, conflict, notFound, readBody, universeFor } from "./common";

const MODEL_PROVIDER_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const ENDPOINT_API_KINDS = new Set<string>(["openai:responses", "openai:completions"]);
const ENVIRONMENT_SECRET_PROVIDER_ID = "environment-secret";
const DEFAULT_GITHUB_API_BASE_URL = "https://api.github.com";
/// `claude setup-token` mints one-year tokens.
const CLAUDE_CODE_TOKEN_TTL_MS = 365 * 24 * 60 * 60 * 1000;
const CONFIGURATOR_VERSION = 4;
const CONFIGURATOR_SERVER_ID = "lightspeed-configurator";
const CONFIGURATOR_PROFILE_ID = "lightspeed-configurator";
const CONFIGURATOR_MCP_URL = "https://configurator.lightspeed.demo/mcp";
const INSTALL_DELAY_MS = 2_000;

/// Installations GitHub would report for any registered App.
/// Installations come from the universe's own GitHub App grants (what the
/// fixtures and the grant route record), plus one ungranted sibling per
/// account so the "grant" flow has something to pick.
function installationsFor(universe: UniverseState, providerId: string): GitHubInstallation[] {
  const granted: GitHubInstallation[] = [];
  for (const grant of universe.secrets.grants) {
    if (grant.providerKind !== "gitHubApp" || grant.status === "revoked") continue;
    if (grant.providerId !== providerId && universe.githubApps.length > 1) continue;
    const metadata = isRecord(grant.metadata) ? grant.metadata : {};
    const installationId = Number(metadata.installation_id);
    if (!Number.isSafeInteger(installationId) || installationId <= 0) continue;
    granted.push({
      installationId,
      accountLogin: optionalString(metadata.account_login) ?? grant.subjectHint ?? null,
      repositorySelection: optionalString(metadata.repository_selection) ?? null,
      permissions: isRecord(metadata.permissions) ? metadata.permissions : {},
    });
  }
  const siblings = granted.map((installation) => ({
    installationId: installation.installationId + 1,
    accountLogin: installation.accountLogin ? `${installation.accountLogin}-staging` : null,
    repositorySelection: "all",
    permissions: { contents: "read", metadata: "read" },
  }));
  return [...granted, ...siblings];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function optionalString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function hex(): string {
  return crypto.randomUUID().replace(/-/g, "");
}

/// The engine namespaces model credentials as `model:<providerId>`.
function credentialIdFor(providerId: string): string {
  return providerId.startsWith("model:") ? providerId : `model:${providerId}`;
}

function findModelProvider(universe: UniverseState, credentialId: string): SecretProvider | undefined {
  return universe.secrets.providers.find((provider) => provider.credentialId === credentialId);
}

function findGrant(universe: UniverseState, grantId: string): SecretGrant | undefined {
  return universe.secrets.grants.find((grant) => grant.grantId === grantId);
}

function credentialIdConflictMessage(grantId: string, status?: string): string {
  if (status === "revoked") {
    return `credential ID "${grantId}" belongs to a revoked access credential and cannot be reused; leave it blank to generate a new ID or choose another`;
  }
  const state = status ? `an access credential with status "${status}"` : "another access credential";
  return `credential ID "${grantId}" already belongs to ${state}; leave it blank to generate a new ID or choose another`;
}

interface GrantInit {
  providerId: string;
  providerKind: SecretGrant["providerKind"];
  grantId?: string;
  displayName?: string | null;
  subjectHint?: string | null;
  exposure?: SecretGrant["exposure"];
  createdBy?: SecretGrant["createdBy"];
  scopes?: string[];
  audience?: string | null;
  expiresAtMs?: number | null;
  hasRefreshToken?: boolean;
  metadata?: Record<string, unknown>;
}

/// `auth/grants/import`: the token is dropped on the floor, the metadata
/// row is what every read returns.
function mintGrant(store: DemoStore, universe: UniverseState, init: GrantInit): SecretGrant {
  const now = Date.now();
  const grant: SecretGrant = {
    grantId: init.grantId ?? store.nextId("authgrant"),
    providerId: init.providerId,
    providerKind: init.providerKind,
    displayName: init.displayName ?? null,
    subjectHint: init.subjectHint ?? null,
    status: "active",
    exposure: init.exposure ?? "brokered",
    createdBy: init.createdBy ?? { kind: "actor", id: store.currentUser.id },
    scopes: init.scopes ?? [],
    audience: init.audience ?? null,
    hasAccessToken: true,
    hasRefreshToken: init.hasRefreshToken ?? false,
    expiresAtMs: init.expiresAtMs ?? null,
    lastLeasedAtMs: null,
    leaseCount: 0,
    metadata: init.metadata ?? {},
    createdAtMs: now,
    updatedAtMs: now,
  };
  universe.secrets.grants.push(grant);
  return grant;
}

function revokeGrant(grant: SecretGrant): SecretGrant {
  if (grant.status !== "revoked") {
    grant.status = "revoked";
    grant.updatedAtMs = Date.now();
  }
  return grant;
}

/// Subscription grants are ordinary bearer grants tagged at import time.
function isSubscriptionGrant(grant: SecretGrant): boolean {
  return grant.providerKind === "staticBearer"
    && (grant.metadata?.subscription === "claudeCode" || grant.metadata?.subscription === "codex");
}

function decodeJwtPayload(token: string): Record<string, unknown> | null {
  const payload = token.split(".")[1];
  if (!payload) return null;
  try {
    const value: unknown = JSON.parse(base64ToText(payload.replace(/-/g, "+").replace(/_/g, "/")));
    return isRecord(value) ? value : null;
  } catch {
    return null;
  }
}

interface ParsedSubscription {
  providerId: string;
  shape: "token" | "codexTokenSet";
  metadata: Record<string, unknown>;
  expiresAtMs?: number;
  subjectHint?: string;
}

/// Vendor-shaped credential parsing (Claude Code `setup-token`, Codex
/// `auth.json`), kept to what the grant's metadata needs.
function parseSubscription(
  provider: string,
  credential: string,
  nowMs: number,
): ParsedSubscription | { error: string } {
  const trimmed = credential.trim();
  if (!trimmed) return { error: "credential is empty" };
  if (provider === "anthropic") {
    if (!trimmed.startsWith("sk-ant-oat") || /\s/.test(trimmed)) {
      return { error: "not a Claude Code token (expected sk-ant-oat… from `claude setup-token`)" };
    }
    return {
      providerId: "anthropic",
      shape: "token",
      metadata: { subscription: "claudeCode", credential: "token", source: "pasted" },
      expiresAtMs: nowMs + CLAUDE_CODE_TOKEN_TTL_MS,
    };
  }
  if (!trimmed.startsWith("{")) {
    if (/\s/.test(trimmed)) {
      return { error: "expected an auth.json document or a single access token" };
    }
    return {
      providerId: "openai",
      shape: "token",
      metadata: { subscription: "codex", credential: "token", source: "pasted" },
    };
  }
  let document: unknown;
  try {
    document = JSON.parse(trimmed);
  } catch (error) {
    return { error: `auth.json is not valid JSON: ${error instanceof Error ? error.message : String(error)}` };
  }
  const tokens = isRecord(document) && isRecord(document.tokens) ? document.tokens : null;
  if (!tokens || typeof tokens.access_token !== "string") {
    return {
      error: "auth.json has no ChatGPT tokens (`tokens.access_token`); API-key-only files are not a subscription credential",
    };
  }
  const claims = typeof tokens.id_token === "string" ? decodeJwtPayload(tokens.id_token) : null;
  const auth = claims && isRecord(claims["https://api.openai.com/auth"])
    ? claims["https://api.openai.com/auth"]
    : {};
  const email = optionalString(claims?.email);
  const accountId = optionalString(tokens.account_id) ?? optionalString(auth.chatgpt_account_id);
  const planType = optionalString(auth.chatgpt_plan_type);
  const access = decodeJwtPayload(tokens.access_token);
  const exp = access && typeof access.exp === "number" ? access.exp * 1000 : undefined;
  return {
    providerId: "openai",
    shape: "codexTokenSet",
    metadata: {
      subscription: "codex",
      credential: "tokenSet",
      source: "pasted",
      ...(email ? { email } : {}),
      ...(accountId ? { accountId } : {}),
      ...(planType ? { planType } : {}),
    },
    expiresAtMs: exp,
    subjectHint: email,
  };
}

function parseEndpoint(value: unknown): ModelEndpointConfig | { error: string } | null {
  if (value === undefined || value === null) return null;
  if (!isRecord(value)) return { error: "endpoint must be an object" };
  const baseUrl = optionalString(value.baseUrl);
  try {
    new URL(baseUrl ?? "");
  } catch {
    return { error: "endpoint baseUrl must be a valid URL" };
  }
  const apiKinds = Array.isArray(value.apiKinds)
    ? value.apiKinds.filter((kind): kind is ModelEndpointConfig["apiKinds"][number] =>
      typeof kind === "string" && ENDPOINT_API_KINDS.has(kind))
    : [];
  if (apiKinds.length === 0) return { error: "endpoint needs at least one API kind" };
  if (new Set(apiKinds).size !== apiKinds.length) return { error: "API kinds must be unique" };
  const headers: Record<string, string> = {};
  if (isRecord(value.headers)) {
    for (const [name, header] of Object.entries(value.headers)) {
      if (typeof header === "string") headers[name] = header;
    }
  }
  return { baseUrl: baseUrl ?? "", apiKinds, ...(Object.keys(headers).length > 0 ? { headers } : {}) };
}

function endpointApiKinds(provider: SecretProvider): string[] {
  const endpoint = provider.config.type === "githubApp" ? null : provider.config.endpoint;
  return endpoint?.apiKinds ?? ["openai:responses"];
}

/// Credential state as `models/list` would report it: a universe row wins,
/// built-in providers may fall back to the deployment's own client, and
/// anything else is missing.
function credentialStatus(
  universe: UniverseState,
  provider: Pick<ModelProviderDiscovery, "providerId" | "credentialSource">,
): Pick<ModelProviderDiscovery, "credential" | "credentialSource" | "error"> {
  const secret = findModelProvider(universe, credentialIdFor(provider.providerId));
  if (secret?.status === "active" && secret.hasCredential) {
    return { credential: "configured", credentialSource: "universe", error: null };
  }
  if (secret?.status === "active" && secret.config.type === "modelEndpoint") {
    return { credential: "notRequired", credentialSource: "universe", error: null };
  }
  const builtIn = provider.providerId === "openai" || provider.providerId === "anthropic";
  if (builtIn && provider.credentialSource === "deployment") {
    return { credential: "configured", credentialSource: "deployment", error: null };
  }
  return { credential: "missing", credentialSource: "none", error: null };
}

function modelDiscovery(universe: UniverseState): ModelListResponse {
  const known = new Set<string>();
  const providers: ModelProviderDiscovery[] = (universe.models.providers ?? []).map((provider) => {
    known.add(provider.providerId);
    return { ...provider, ...credentialStatus(universe, provider) };
  });
  // Providers added on Models that the fixture never listed
  // show up configured, with nothing discovered from them yet.
  for (const secret of universe.secrets.providers) {
    if (known.has(secret.providerId) || !secret.usableForModels || secret.status !== "active") continue;
    known.add(secret.providerId);
    providers.push({
      providerId: secret.providerId,
      apiKinds: endpointApiKinds(secret),
      fetchedAtMs: Date.now(),
      ...credentialStatus(universe, { providerId: secret.providerId, credentialSource: "universe" }),
    });
  }
  // Models discovered through a provider that lost its credential go with
  // it; models of providers the fixture never described stay listed.
  const unusable = new Set(
    providers
      .filter((provider) => provider.credential !== "configured" && provider.credential !== "notRequired")
      .map((provider) => provider.providerId),
  );
  return {
    models: (universe.models.models ?? []).filter((model) => !unusable.has(model.providerId)),
    providers,
  };
}

function configuratorSetup(universe: UniverseState): UniverseSetup {
  let setup = universe.setups.find((candidate) => candidate.id === "configurator");
  if (!setup) {
    setup = {
      id: "configurator",
      name: "Configurator",
      description:
        "Creates a dedicated credential, registers the Configurator MCP server, and adds a ready-to-use profile for managing this universe. " +
        "Anyone who can attach the server configures profiles, MCP servers, environments, bots, channels, credentials and models with its key.",
      version: CONFIGURATOR_VERSION,
      available: true,
      status: "available",
    };
    universe.setups.push(setup);
  }
  return setup;
}

/// What the real install leaves behind — an API key, its bearer grant, the
/// MCP server, and the profile — so the other pages show the result.
function finishConfiguratorInstall(store: DemoStore, universe: UniverseState, setup: UniverseSetup): void {
  const now = Date.now();
  const keyPrefix = `lsk_${hex().slice(0, 8)}`;
  universe.apiKeys.push({
    keyPrefix,
    displayName: "Lightspeed Configurator setup",
    createdAtMs: now,
    revokedAtMs: null,
    lastUsedAtMs: null,
  });
  const grant = mintGrant(store, universe, {
    grantId: `authgrant_lightspeed_configurator_${hex()}`,
    providerId: "lightspeed-configurator",
    providerKind: "staticBearer",
    displayName: "Lightspeed Configurator setup",
    audience: CONFIGURATOR_MCP_URL,
  });
  const existingServer = universe.mcpServers.get(CONFIGURATOR_SERVER_ID);
  const server: McpServer = {
    serverId: CONFIGURATOR_SERVER_ID,
    displayName: "Lightspeed Configurator",
    serverUrl: CONFIGURATOR_MCP_URL,
    defaultServerLabel: "configurator",
    description: "Configure and operate this Lightspeed universe through its generated API.",
    allowedTools: null,
    execution: "native",
    exposure: "search",
    approval: "never",
    deferLoading: null,
    allowPrivateNetwork: false,
    authPolicy: { type: "requiredBearer" },
    credential: { type: "authGrant", grantId: grant.grantId },
    status: "active",
    revision: (existingServer?.revision ?? 0) + 1,
    createdAtMs: existingServer?.createdAtMs ?? now,
    updatedAtMs: now,
  };
  universe.mcpServers.set(server.serverId, server);
  const existingProfile = universe.profiles.get(CONFIGURATOR_PROFILE_ID);
  const profile: ProfileDocument = {
    profileId: CONFIGURATOR_PROFILE_ID,
    displayName: "Universe Configurator",
    description: "A ready-to-use agent profile for configuring and operating this universe.",
    instructions: {
      type: "text",
      text:
        "Configure and operate the current Lightspeed universe. Read revisioned resources before replacing them, make only the requested changes, and report the resulting resource identifiers.",
    },
    config: { features: { mcp: { servers: [{ serverId: CONFIGURATOR_SERVER_ID }] } } },
    revision: (existingProfile?.revision ?? 0) + 1,
    createdAtMs: existingProfile?.createdAtMs ?? now,
    updatedAtMs: now,
  };
  universe.profiles.set(profile.profileId, profile);
  setup.status = "ready";
  setup.installedVersion = CONFIGURATOR_VERSION;
  setup.resources = {
    keyPrefix,
    grantId: grant.grantId,
    serverId: CONFIGURATOR_SERVER_ID,
    profileId: CONFIGURATOR_PROFILE_ID,
  };
}

export function secretRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.get("/:id/secrets", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json(universe.secrets);
  });

  app.post("/:id/secrets/providers", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{ providerId?: unknown; displayName?: unknown; credential?: unknown }>(c);
    const providerId = body.providerId;
    if (providerId !== "openai" && providerId !== "anthropic") {
      return badRequest(c, "providerId must be openai or anthropic");
    }
    if (typeof body.credential !== "string" || !body.credential) {
      return badRequest(c, "credential is required");
    }
    const credentialId = credentialIdFor(providerId);
    if (findModelProvider(universe, credentialId)) {
      return conflict(c, `model provider ${providerId} already has a credential`);
    }
    const now = Date.now();
    const provider: SecretProvider = {
      providerId,
      credentialId,
      usableForModels: true,
      providerKind: "modelApiKey",
      displayName: optionalString(body.displayName) ?? null,
      config: { type: "modelApiKey" },
      hasCredential: true,
      status: "active",
      createdAtMs: now,
      updatedAtMs: now,
    };
    universe.secrets.providers.push(provider);
    return c.json(provider, 201);
  });

  app.delete("/:id/secrets/providers/:providerId", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const index = universe.secrets.providers.findIndex(
      (provider) => provider.credentialId === c.req.param("providerId"),
    );
    if (index < 0) return notFound(c);
    const [provider] = universe.secrets.providers.splice(index, 1);
    return c.json(provider);
  });

  app.post("/:id/secrets/grants", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{
      grantId?: unknown;
      displayName?: unknown;
      subjectHint?: unknown;
      exposure?: unknown;
      token?: unknown;
    }>(c);
    if (typeof body.token !== "string" || !body.token) return badRequest(c, "token is required");
    const grantId = optionalString(body.grantId);
    const existing = grantId ? findGrant(universe, grantId) : undefined;
    if (grantId && existing) return conflict(c, credentialIdConflictMessage(grantId, existing.status));
    const grant = mintGrant(store, universe, {
      grantId,
      providerId: "static",
      providerKind: "staticBearer",
      displayName: optionalString(body.displayName),
      subjectHint: optionalString(body.subjectHint),
      exposure: body.exposure === "retrievable" ? "retrievable" : "brokered",
    });
    return c.json(grant, 201);
  });

  /// Opaque, multiline-safe values for environment-variable injection; the
  /// dedicated provider id keeps them distinct from bearer tokens.
  app.post("/:id/secrets/environment", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{ grantId?: unknown; displayName?: unknown; value?: unknown }>(c);
    if (typeof body.value !== "string" || !body.value) return badRequest(c, "value is required");
    if (body.value.includes("\0")) return badRequest(c, "environment secrets cannot contain NUL bytes");
    const grantId = optionalString(body.grantId);
    const existing = grantId ? findGrant(universe, grantId) : undefined;
    if (grantId && existing) return conflict(c, credentialIdConflictMessage(grantId, existing.status));
    const grant = mintGrant(store, universe, {
      grantId,
      providerId: ENVIRONMENT_SECRET_PROVIDER_ID,
      providerKind: "staticBearer",
      displayName: optionalString(body.displayName),
    });
    return c.json(grant, 201);
  });

  app.delete("/:id/secrets/grants/:grantId", (c) => {
    const universe = universeFor(store, c);
    const grant = universe ? findGrant(universe, c.req.param("grantId")) : undefined;
    if (!grant) return notFound(c);
    return c.json(revokeGrant(grant));
  });

  /// Active grants for MCP-server credential selection.
  app.get("/:id/auth-grants", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json(universe.secrets.grants.filter((grant) => grant.status === "active"));
  });

  app.get("/:id/integrations/github", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json({
      apps: universe.githubApps,
      grants: universe.secrets.grants.filter((grant) => grant.providerKind === "gitHubApp"),
    });
  });

  app.post("/:id/integrations/github/apps", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{
      providerId?: unknown;
      displayName?: unknown;
      appId?: unknown;
      apiBaseUrl?: unknown;
      privateKey?: unknown;
    }>(c);
    const appId = optionalString(body.appId);
    if (!appId || !/^[0-9]+$/.test(appId)) return badRequest(c, "GitHub App ID must be numeric");
    if (typeof body.privateKey !== "string" || !body.privateKey) {
      return badRequest(c, "privateKey is required");
    }
    const providerId = optionalString(body.providerId) ?? `github-app:${appId}`;
    if (universe.githubApps.some((app) => app.providerId === providerId)) {
      return conflict(c, `auth provider ${providerId} already exists`);
    }
    const now = Date.now();
    const app: GitHubApp = {
      providerId,
      providerKind: "gitHubApp",
      displayName: optionalString(body.displayName) ?? null,
      config: {
        type: "githubApp",
        appId,
        apiBaseUrl: optionalString(body.apiBaseUrl) ?? DEFAULT_GITHUB_API_BASE_URL,
      },
      hasCredential: true,
      status: "active",
      createdAtMs: now,
      updatedAtMs: now,
    };
    universe.githubApps.push(app);
    return c.json(app, 201);
  });

  app.delete("/:id/integrations/github/apps/:providerId", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const index = universe.githubApps.findIndex((app) => app.providerId === c.req.param("providerId"));
    if (index < 0) return notFound(c);
    const [app] = universe.githubApps.splice(index, 1);
    return c.json(app);
  });

  app.get("/:id/integrations/github/apps/:providerId/installations", (c) => {
    const universe = universeFor(store, c);
    const app = universe?.githubApps.find((candidate) => candidate.providerId === c.req.param("providerId"));
    if (!universe || !app) return notFound(c);
    return c.json(installationsFor(universe, app.providerId));
  });

  app.post(
    "/:id/integrations/github/apps/:providerId/installations/:installationId/grant",
    async (c) => {
      const universe = universeFor(store, c);
      const app = universe?.githubApps.find((candidate) => candidate.providerId === c.req.param("providerId"));
      if (!universe || !app) return notFound(c);
      const installationId = Number(c.req.param("installationId"));
      if (!Number.isSafeInteger(installationId) || installationId <= 0) {
        return badRequest(c, "installationId must be a positive integer");
      }
      const body = await readBody<{ displayName?: unknown }>(c);
      const installation = installationsFor(universe, app.providerId).find(
        (candidate) => candidate.installationId === installationId,
      );
      const login = installation?.accountLogin ?? null;
      const grant = mintGrant(store, universe, {
        providerId: app.providerId,
        providerKind: "gitHubApp",
        displayName: optionalString(body.displayName) ?? (login ? `GitHub: ${login}` : null),
        subjectHint: login,
        expiresAtMs: Date.now() + 3_600_000,
        metadata: {
          installation_id: installationId,
          account_login: login,
          repository_selection: installation?.repositorySelection ?? null,
          permissions: installation?.permissions ?? {},
        },
      });
      return c.json(grant, 201);
    },
  );

  app.get("/:id/integrations/subscriptions", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json(universe.secrets.grants.filter(isSubscriptionGrant));
  });

  app.post("/:id/integrations/subscriptions", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{ provider?: unknown; credential?: unknown; displayName?: unknown }>(c);
    if (body.provider !== "anthropic" && body.provider !== "openAi") {
      return badRequest(c, "provider must be anthropic or openAi");
    }
    if (typeof body.credential !== "string" || !body.credential) {
      return badRequest(c, "credential is required");
    }
    const parsed = parseSubscription(body.provider, body.credential, Date.now());
    if ("error" in parsed) return badRequest(c, parsed.error);
    const grant = mintGrant(store, universe, {
      providerId: parsed.providerId,
      providerKind: "staticBearer",
      displayName: optionalString(body.displayName),
      subjectHint: parsed.subjectHint,
      expiresAtMs: parsed.expiresAtMs,
      metadata: parsed.metadata,
    });
    return c.json({ grant, shape: parsed.shape }, 201);
  });

  app.delete("/:id/integrations/subscriptions/:grantId", (c) => {
    const universe = universeFor(store, c);
    const grant = universe ? findGrant(universe, c.req.param("grantId")) : undefined;
    if (!grant) return notFound(c);
    return c.json(revokeGrant(grant));
  });

  /// Model provider keys and OpenAI-compatible endpoints (`model:<provider>`
  /// rows). `replace` swaps the row in place.
  app.post("/:id/integrations/model-keys", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{
      provider?: unknown;
      credential?: unknown;
      endpoint?: unknown;
      displayName?: unknown;
      replace?: unknown;
    }>(c);
    const providerId = optionalString(body.provider) ?? "";
    if (!MODEL_PROVIDER_ID.test(providerId)) return badRequest(c, "provider id is invalid");
    const credential = typeof body.credential === "string" && body.credential ? body.credential : null;
    const endpoint = parseEndpoint(body.endpoint);
    if (endpoint && "error" in endpoint) return badRequest(c, endpoint.error);
    const builtIn = providerId === "openai" || providerId === "anthropic";
    if (!builtIn && !endpoint) return badRequest(c, "custom model providers require an endpoint");
    if (providerId === "anthropic" && endpoint) {
      return badRequest(c, "Anthropic-compatible endpoint overrides are not supported");
    }
    if (!credential && !endpoint) return badRequest(c, "a model provider requires a credential or endpoint");
    const credentialId = credentialIdFor(providerId);
    const existing = findModelProvider(universe, credentialId);
    if (existing && body.replace !== true) {
      return conflict(c, `model provider ${providerId} already has a credential; replace it instead`);
    }
    const config: SecretProvider["config"] = endpoint
      ? credential
        ? { type: "modelApiKey", endpoint }
        : { type: "modelEndpoint", endpoint }
      : { type: "modelApiKey" };
    const now = Date.now();
    const provider: SecretProvider = {
      providerId,
      credentialId,
      usableForModels: true,
      providerKind: config.type === "modelEndpoint" ? "modelEndpoint" : "modelApiKey",
      displayName: optionalString(body.displayName) ?? null,
      config,
      hasCredential: credential !== null,
      status: "active",
      createdAtMs: existing?.createdAtMs ?? now,
      updatedAtMs: now,
    };
    if (existing) {
      universe.secrets.providers.splice(universe.secrets.providers.indexOf(existing), 1, provider);
    } else {
      universe.secrets.providers.push(provider);
    }
    return c.json(provider, 201);
  });

  app.get("/:id/models", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json(modelDiscovery(universe));
  });

  /// Operators see the templates; installing stays with Admins.
  app.get("/:id/setups", (c) => {
    const universe = universeFor(store, c);
    const role = universe?.universe.role;
    if (!universe || (role !== "admin" && role !== "operator")) return notFound(c);
    configuratorSetup(universe);
    return c.json(universe.setups);
  });

  /// Accepted as `installing`; the page polls until the resources land.
  app.post("/:id/setups/configurator/install", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    if (universe.universe.role !== "admin") return c.json({ error: "universe admin required" }, 403);
    const setup = configuratorSetup(universe);
    if (!setup.available) return c.json({ error: "Configurator MCP URL is not configured" }, 501);
    if (setup.status === "installing") {
      return conflict(c, "Configurator setup installation is already running");
    }
    setup.status = "installing";
    delete setup.error;
    setTimeout(() => finishConfiguratorInstall(store, universe, setup), INSTALL_DELAY_MS);
    return c.json(setup);
  });

  return app;
}
