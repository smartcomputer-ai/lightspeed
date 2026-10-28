/// Universe environments: provisioned, external and registered records, the
/// provider bindings and templates they come from, credential bindings, and a
/// simulated provider lifecycle (provisioning → open, closing → closed) so
/// the Environments page has state to watch. Power and ingress are
/// synchronous provider calls, as in the core.
import { Hono } from "hono";
import type { Context } from "hono";
import type {
  EnvironmentIdlePolicyView,
  EnvironmentPowerStateView,
  EnvironmentStateView,
} from "@lightspeed-ai/sdk";
import type {
  Environment,
  EnvironmentCredential,
  EnvironmentCredentialSource,
  EnvironmentRegistrationKey,
} from "@/api";
import { displayStatus } from "../fixtures/builders";
import type { DemoStore, UniverseState } from "../store";
import { badRequest, conflict, notFound, readBody, universeFor } from "./common";

const IDLE_STAGES = ["pauseAfterMs", "suspendAfterMs", "stopAfterMs", "closeAfterMs"] as const;
const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]{0,127}$/;
const ENVD_ENDPOINT = /^(wss?:\/\/|unix:\/\/\/)[^\s]+$/;
const DEV_ENVD_ENDPOINT = "ws://127.0.0.1:19091";
const DEMO_DAEMON = { version: "0.1.0", protocolVersion: 4 };
const LIST_DEFAULT_LIMIT = 100;

export interface ProvisionParams {
  requestId: string;
  providerId: string;
  templateId: string;
  displayName?: string | null;
  idle?: EnvironmentIdlePolicyView | null;
  metadata?: Record<string, unknown>;
}

/// Recompute the derived display status after any write, the way the core's
/// view derives it on every read.
function settle(environment: Environment, now = Date.now()): Environment {
  environment.status = displayStatus(environment);
  environment.updatedAtMs = now;
  return environment;
}

function setState(environment: Environment, state: EnvironmentStateView, now = Date.now()): void {
  environment.state = state;
  environment.stateChangedAtMs = now;
  if (state === "closed") {
    environment.closedAtMs = now;
    environment.stateMessage = null;
  }
  settle(environment, now);
}

/// Pending simulated provider work per environment. A close bumps the epoch
/// so stale provisioning timers drop instead of reopening the row.
const epochs = new WeakMap<Environment, number>();

function schedule(environment: Environment, steps: Array<[number, () => void]>): void {
  const epoch = (epochs.get(environment) ?? 0) + 1;
  epochs.set(environment, epoch);
  for (const [delayMs, step] of steps) {
    setTimeout(() => {
      if (epochs.get(environment) !== epoch || environment.state === "closed") return;
      step();
    }, delayMs);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isPowerState(value: unknown): value is EnvironmentPowerStateView {
  return value === "running" || value === "paused" || value === "suspended" || value === "stopped";
}

/// Only string values survive: the engine's metadata is a string map.
function stringMetadata(value: unknown): Record<string, string> {
  const metadata: Record<string, string> = {};
  if (!isRecord(value)) return metadata;
  for (const [key, entry] of Object.entries(value)) {
    if (typeof entry === "string") metadata[key] = entry;
  }
  return metadata;
}

/// The protocol's idle-policy rules: at least one stage, every threshold a
/// positive integer, non-decreasing pause → suspend → stop → close.
function parseIdlePolicy(value: unknown): { policy: EnvironmentIdlePolicyView | null } | { error: string } {
  if (value === undefined || value === null) return { policy: null };
  if (!isRecord(value)) return { error: "idle policy must be an object" };
  const policy: EnvironmentIdlePolicyView = {};
  let previous: [string, number] | null = null;
  for (const key of IDLE_STAGES) {
    const threshold = value[key];
    if (threshold === undefined || threshold === null) continue;
    if (typeof threshold !== "number" || !Number.isInteger(threshold) || threshold <= 0) {
      return { error: `idle policy ${key} threshold must be positive` };
    }
    if (previous && threshold < previous[1]) {
      return { error: `idle policy ${key} threshold must not be below ${previous[0]}` };
    }
    policy[key] = threshold;
    previous = [key, threshold];
  }
  if (!previous) return { error: "idle policy sets no stage" };
  return { policy };
}

function findByRequestId(universe: UniverseState, requestId: string): Environment | undefined {
  for (const environment of universe.environments.values()) {
    if (environment.requestId === requestId) return environment;
  }
  return undefined;
}

/// `environments/create` semantics: the request id dedupes inside the
/// universe, the binding must be enabled, and the record is accepted before
/// any provider work. An unknown template fails asynchronously, the way a
/// provider refusal of `target/create` does.
export function provisionEnvironment(
  store: DemoStore,
  universe: UniverseState,
  params: ProvisionParams,
): { environment: Environment; created: boolean } | { error: string } {
  const existing = findByRequestId(universe, params.requestId);
  if (existing) return { environment: existing, created: false };
  const binding = universe.providerBindings.find((candidate) => candidate.providerId === params.providerId);
  if (!binding) return { error: `environment provider binding not found: ${params.providerId}` };
  if (!binding.enabled) return { error: `environment provider binding is disabled: ${params.providerId}` };
  const template = universe.environmentTemplates.find(
    (candidate) => candidate.providerId === params.providerId && candidate.templateId === params.templateId,
  );
  const environmentId = store.nextId("env");
  const now = Date.now();
  const environment: Environment = {
    environmentId,
    requestId: params.requestId,
    source: {
      type: "provisioned",
      providerId: params.providerId,
      templateId: params.templateId,
      ...(params.idle ? { idle: params.idle } : {}),
    },
    displayName: params.displayName ?? null,
    state: "provisioning",
    status: "provisioning",
    stateChangedAtMs: now,
    metadata: stringMetadata(params.metadata),
    createdAtMs: now,
    updatedAtMs: now,
  };
  universe.environments.set(environmentId, environment);
  const source = environment.source;
  if (source.type !== "provisioned") throw new Error("unreachable");
  if (template) {
    schedule(environment, [
      [2_500, () => {
        source.targetId = `ls-${environmentId}`;
        source.observed = { phase: "provisioning", message: "booting", observedAtMs: Date.now() };
        environment.stateMessage = "waiting for the provider: booting";
        settle(environment);
      }],
      [6_000, () => {
        source.observed = { phase: "active", power: "running", observedAtMs: Date.now() };
        environment.stateMessage = null;
        environment.daemon = DEMO_DAEMON;
        setState(environment, "open");
      }],
    ]);
  } else {
    schedule(environment, [[1_500, () => {
      environment.stateMessage = `the provider refused target/create: unknown template ${params.templateId}`;
      setState(environment, "failed");
    }]]);
  }
  return { environment, created: true };
}

/// `environments/close`: a provisioned environment is closing until the
/// provider reports its target gone; external and registered ones close at
/// once.
export function closeEnvironment(
  universe: UniverseState,
  environmentId: string,
  reason: NonNullable<Environment["closeReason"]> = "requested",
): Environment | null {
  const environment = universe.environments.get(environmentId);
  if (!environment) return null;
  if (environment.state === "closed" || environment.state === "closing") return environment;
  environment.closeReason = reason;
  const source = environment.source;
  if (source.type !== "provisioned") {
    setState(environment, "closed");
    return environment;
  }
  environment.stateMessage = null;
  setState(environment, "closing");
  schedule(environment, [[2_000, () => {
    source.observed = { phase: "closed", observedAtMs: Date.now() };
    source.publicEndpoint = null;
    setState(environment, "closed");
  }]]);
  return environment;
}

/// Stable, id-safe request id for an external endpoint so re-registering
/// the same daemon converges on one environment.
function externalRequestId(endpoint: string): string {
  const slug = endpoint
    .replace(/^wss?:\/\//, "")
    .replace(/\/+$/, "")
    .replace(/[^A-Za-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 100);
  return `external-${slug || "envd"}`;
}

/// Claude Code prefers `ANTHROPIC_API_KEY`/`ANTHROPIC_AUTH_TOKEN` over
/// `CLAUDE_CODE_OAUTH_TOKEN`; binding both silently disables the
/// subscription, so the pair is refused.
function conflictingAnthropicEnv(newName: string, existing: string[]): string | null {
  const keys = ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];
  if (newName === "CLAUDE_CODE_OAUTH_TOKEN") {
    return existing.find((name) => keys.includes(name)) ?? null;
  }
  if (keys.includes(newName)) {
    return existing.find((name) => name === "CLAUDE_CODE_OAUTH_TOKEN") ?? null;
  }
  return null;
}

function parseCredentialSource(value: unknown): EnvironmentCredentialSource | null {
  if (!isRecord(value)) return null;
  if (value.type === "authGrant" && typeof value.grantId === "string" && value.grantId) {
    return { type: "authGrant", grantId: value.grantId };
  }
  if (
    value.type === "authProviderCredential"
    && typeof value.providerId === "string"
    && value.providerId
  ) {
    return { type: "authProviderCredential", providerId: value.providerId };
  }
  return null;
}

/// Whether the source resolves to something injectable right now: an active
/// grant, or an active model provider row that holds a credential.
function credentialSourceAvailable(universe: UniverseState, source: EnvironmentCredentialSource): boolean {
  if (source.type === "authGrant") {
    return universe.secrets.grants.some(
      (grant) => grant.grantId === source.grantId && grant.status === "active",
    );
  }
  if (source.type === "authProviderCredential") {
    return universe.secrets.providers.some(
      (provider) =>
        provider.credentialId === source.providerId
        && provider.status === "active"
        && provider.hasCredential,
    );
  }
  return true;
}

function locate(
  store: DemoStore,
  c: Context,
): { universe: UniverseState; environment: Environment } | null {
  const universe = universeFor(store, c);
  const environment = universe?.environments.get(c.req.param("environmentId") ?? "");
  return universe && environment ? { universe, environment } : null;
}

type RegistrationKeyBody = {
  displayName?: string;
  identityMode?: "persistent" | "ephemeral";
  maxActiveEnvironments?: number;
  ephemeralDisconnectGraceMs?: number;
  offlineCloseAfterMs?: number;
  expiresAtMs?: number;
};

/// The editable policy of `registration-keys/create` and `/update`: a full
/// replace, so an absent field is unset.
function registrationKeyPolicy(
  body: RegistrationKeyBody,
  identityMode: "persistent" | "ephemeral",
): Pick<
  EnvironmentRegistrationKey,
  "displayName" | "maxActiveEnvironments" | "ephemeralDisconnectGraceMs" | "offlineCloseAfterMs" | "expiresAtMs"
> | { error: string } {
  const displayName = body.displayName?.trim();
  if (!displayName) return { error: "displayName is required" };
  if (body.offlineCloseAfterMs !== undefined && identityMode === "ephemeral") {
    return { error: "offlineCloseAfterMs applies to persistent keys only" };
  }
  return {
    displayName,
    maxActiveEnvironments: body.maxActiveEnvironments ?? null,
    ephemeralDisconnectGraceMs: body.ephemeralDisconnectGraceMs ?? 300_000,
    offlineCloseAfterMs: body.offlineCloseAfterMs ?? null,
    expiresAtMs: body.expiresAtMs ?? null,
  };
}

function mintSecret(): string {
  return `lsrk_${crypto.randomUUID().replace(/-/g, "")}${crypto.randomUUID().replace(/-/g, "").slice(0, 11)}`;
}

export function environmentRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.get("/:id/environment-provider-bindings", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json(universe.providerBindings);
  });

  /// Templates of enabled bindings only, like `environments/templates/list`.
  app.get("/:id/environment-templates", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const providerId = c.req.query("providerId");
    const enabled = new Set(
      universe.providerBindings.filter((binding) => binding.enabled).map((binding) => binding.providerId),
    );
    return c.json(
      universe.environmentTemplates.filter(
        (template) => enabled.has(template.providerId) && (!providerId || template.providerId === providerId),
      ),
    );
  });

  /// One page of `environments/list`, in environment id order; closed rows
  /// only with `includeClosed` or `state=closed`.
  app.get("/:id/environments", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const providerId = c.req.query("providerId");
    const state = c.req.query("state");
    const registrationKeyId = c.req.query("registrationKeyId");
    const includeClosed = c.req.query("includeClosed") === "true" || state === "closed";
    const after = c.req.query("after");
    const limit = Math.min(Math.max(Number(c.req.query("limit") ?? LIST_DEFAULT_LIMIT) || LIST_DEFAULT_LIMIT, 1), 500);
    const matching = [...universe.environments.values()]
      .filter((environment) => {
        const source = environment.source;
        return (includeClosed || environment.state !== "closed")
          && (!providerId || (source.type === "provisioned" && source.providerId === providerId))
          && (!state || environment.state === state)
          && (!registrationKeyId
            || (source.type === "registered" && source.registrationKeyId === registrationKeyId))
          && (!after || environment.environmentId > after);
      })
      .sort((a, b) => (a.environmentId < b.environmentId ? -1 : a.environmentId > b.environmentId ? 1 : 0));
    const environments = matching.slice(0, limit);
    return c.json({
      environments,
      nextAfter: matching.length > limit ? environments[environments.length - 1]!.environmentId : null,
    });
  });

  app.get("/:id/environment-registration-keys", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json(universe.registrationKeys.map((key) => withUsage(universe, key)));
  });

  app.post("/:id/environment-registration-keys", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<RegistrationKeyBody>(c);
    const identityMode = body.identityMode === "ephemeral" ? "ephemeral" : "persistent";
    const policy = registrationKeyPolicy(body, identityMode);
    if ("error" in policy) return badRequest(c, policy.error);
    const now = Date.now();
    const secret = mintSecret();
    const key: EnvironmentRegistrationKey = {
      registrationKeyId: `registration_key_${crypto.randomUUID().replace(/-/g, "")}`,
      keyPrefix: secret.slice(0, 12),
      identityMode,
      ...policy,
      status: "active",
      registeredEnvironmentCount: 0,
      activeEnvironmentCount: 0,
      createdAtMs: now,
      updatedAtMs: now,
    };
    universe.registrationKeys.push(key);
    return c.json({ registrationKey: withUsage(universe, key), secret }, 201);
  });

  app.put("/:id/environment-registration-keys/:keyId", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const key = universe.registrationKeys.find((k) => k.registrationKeyId === c.req.param("keyId"));
    if (!key) return notFound(c);
    const policy = registrationKeyPolicy(await readBody<RegistrationKeyBody>(c), key.identityMode);
    if ("error" in policy) return badRequest(c, policy.error);
    Object.assign(key, policy, { updatedAtMs: Date.now() });
    return c.json(withUsage(universe, key));
  });

  /// Retires the live secret; daemons already admitted keep reconnecting.
  app.post("/:id/environment-registration-keys/:keyId/rotate", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const key = universe.registrationKeys.find((k) => k.registrationKeyId === c.req.param("keyId"));
    if (!key) return notFound(c);
    if (key.status === "revoked") return conflict(c, "registration key unavailable: revoked");
    const secret = mintSecret();
    key.keyPrefix = secret.slice(0, 12);
    key.updatedAtMs = Date.now();
    return c.json({ registrationKey: withUsage(universe, key), secret });
  });

  app.post("/:id/environment-registration-keys/:keyId/revoke", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const key = universe.registrationKeys.find((k) => k.registrationKeyId === c.req.param("keyId"));
    if (!key) return notFound(c);
    const body = await readBody<{ closeEnvironments?: boolean }>(c);
    if (key.status === "active") {
      key.status = "revoked";
      key.revokedAtMs = Date.now();
    }
    const closedEnvironmentIds: string[] = [];
    if (body.closeEnvironments) {
      for (const environment of universe.environments.values()) {
        if (
          environment.source.type === "registered"
          && environment.source.registrationKeyId === key.registrationKeyId
          && environment.state !== "closed"
        ) {
          closeEnvironment(universe, environment.environmentId, "keyRevoked");
          closedEnvironmentIds.push(environment.environmentId);
        }
      }
    }
    return c.json({ registrationKey: withUsage(universe, key), closedEnvironmentIds });
  });

  app.post("/:id/environments", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{
      requestId?: unknown;
      providerId?: unknown;
      templateId?: unknown;
      displayName?: unknown;
      metadata?: unknown;
      idle?: unknown;
    }>(c);
    const requestId = typeof body.requestId === "string" ? body.requestId.trim() : "";
    const providerId = typeof body.providerId === "string" ? body.providerId.trim() : "";
    const templateId = typeof body.templateId === "string" ? body.templateId.trim() : "";
    if (!requestId || !providerId || !templateId) {
      return badRequest(c, "requestId, providerId, and templateId are required");
    }
    const idle = parseIdlePolicy(body.idle);
    if ("error" in idle) return badRequest(c, idle.error);
    const result = provisionEnvironment(store, universe, {
      requestId,
      providerId,
      templateId,
      displayName: typeof body.displayName === "string" && body.displayName.trim()
        ? body.displayName.trim()
        : null,
      idle: idle.policy,
      metadata: isRecord(body.metadata) ? body.metadata : {},
    });
    if ("error" in result) return conflict(c, result.error);
    return c.json(result.environment, 201);
  });

  /// A directly attached `lightspeed-envd`: no provider, open at once.
  app.post("/:id/environments/external", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{ endpoint?: unknown; displayName?: unknown }>(c);
    const endpoint = typeof body.endpoint === "string" ? body.endpoint.trim() : "";
    if (!ENVD_ENDPOINT.test(endpoint)) {
      return badRequest(c, "endpoint must be a ws://, wss:// or unix:// URL");
    }
    const requestId = externalRequestId(endpoint);
    const existing = findByRequestId(universe, requestId);
    if (existing) return c.json(existing, 201);
    const environmentId = store.nextId("env-ext");
    const now = Date.now();
    const environment: Environment = {
      environmentId,
      requestId,
      source: { type: "external", endpoint },
      displayName: typeof body.displayName === "string" && body.displayName.trim()
        ? body.displayName.trim()
        : null,
      state: "open",
      status: "ready",
      stateChangedAtMs: now,
      metadata: {},
      createdAtMs: now,
      updatedAtMs: now,
    };
    universe.environments.set(environmentId, environment);
    return c.json(environment, 201);
  });

  /// Before `/:environmentId` so the literal segment wins.
  app.get("/:id/environments/hints", (c) => {
    if (!universeFor(store, c)) return notFound(c);
    return c.json({ devEnvdEndpoint: DEV_ENVD_ENDPOINT });
  });

  app.get("/:id/environments/:environmentId", (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    return c.json(found.environment);
  });

  /// `environments/update`: a full replace of name and caller metadata.
  app.patch("/:id/environments/:environmentId", async (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    const { environment } = found;
    if (environment.state === "closed") return badRequest(c, "environment closed");
    const body = await readBody<{ displayName?: unknown; metadata?: unknown }>(c);
    environment.displayName = typeof body.displayName === "string" && body.displayName.trim()
      ? body.displayName.trim()
      : null;
    environment.metadata = stringMetadata(body.metadata);
    return c.json(settle(environment));
  });

  app.delete("/:id/environments/:environmentId", (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    return c.json(closeEnvironment(found.universe, found.environment.environmentId));
  });

  /// `environments/power/put`: a synchronous provider call; only the
  /// returned observation is kept.
  app.put("/:id/environments/:environmentId/power", async (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    const { universe, environment } = found;
    const body = await readBody<{ power?: unknown }>(c);
    const power = body.power;
    if (!isPowerState(power)) {
      return badRequest(c, "power must be one of running, paused, suspended, stopped");
    }
    const source = environment.source;
    if (source.type !== "provisioned") {
      return badRequest(c, "only provisioned environments have provider power control");
    }
    if (environment.state !== "open") return badRequest(c, `environment is ${environment.state}`);
    const template = universe.environmentTemplates.find(
      (candidate) => candidate.providerId === source.providerId && candidate.templateId === source.templateId,
    );
    if (!template?.powerStates.includes(power)) {
      return badRequest(c, `the template does not support power state ${power}`);
    }
    source.observed = { phase: "active", power, observedAtMs: Date.now() };
    return c.json(settle(environment));
  });

  /// `environments/ingress/put`: a synchronous provider call that returns
  /// the public endpoint.
  app.put("/:id/environments/:environmentId/ingress", async (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    const { universe, environment } = found;
    const body = await readBody<{ enabled?: unknown }>(c);
    if (typeof body.enabled !== "boolean") return badRequest(c, "enabled must be a boolean");
    const source = environment.source;
    if (source.type !== "provisioned") {
      return badRequest(c, "public ingress is available only for provisioned environments");
    }
    if (environment.state !== "open") return badRequest(c, `environment is ${environment.state}`);
    const template = universe.environmentTemplates.find(
      (candidate) => candidate.providerId === source.providerId && candidate.templateId === source.templateId,
    );
    if (body.enabled && !template?.publicIngress) {
      return badRequest(c, "provider template does not permit public ingress");
    }
    source.publicEndpoint = body.enabled
      ? `https://${environment.environmentId.replace(/[^a-z0-9]/gi, "")}.env.lightspeed.demo`
      : null;
    return c.json(settle(environment));
  });

  app.get("/:id/environments/:environmentId/credentials", (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    const credentials = found.universe.environmentCredentials
      .filter((credential) => credential.environmentId === found.environment.environmentId)
      .sort((a, b) => a.envName.localeCompare(b.envName));
    return c.json(credentials);
  });

  app.post("/:id/environments/:environmentId/credentials", async (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    const { universe, environment } = found;
    const body = await readBody<{ envName?: unknown; source?: unknown }>(c);
    const envName = typeof body.envName === "string" ? body.envName.trim() : "";
    if (!ENV_NAME.test(envName)) return badRequest(c, "invalid environment variable name");
    const source = parseCredentialSource(body.source);
    if (!source) return badRequest(c, "source must be an authGrant or authProviderCredential");
    if (!credentialSourceAvailable(universe, source)) return notFound(c, "credential source not found");
    const bound = universe.environmentCredentials.filter(
      (credential) => credential.environmentId === environment.environmentId,
    );
    const clash = conflictingAnthropicEnv(envName, bound.map((credential) => credential.envName));
    if (clash) {
      return conflict(
        c,
        `${envName} cannot be bound alongside ${clash}: Claude Code prefers the API key and would ignore the subscription token; unbind one first`,
      );
    }
    const now = Date.now();
    const existing = bound.find((credential) => credential.envName === envName);
    const credential: EnvironmentCredential = {
      environmentId: environment.environmentId,
      envName,
      source,
      createdAtMs: existing?.createdAtMs ?? now,
      updatedAtMs: now,
    };
    if (existing) {
      universe.environmentCredentials.splice(universe.environmentCredentials.indexOf(existing), 1);
    }
    universe.environmentCredentials.push(credential);
    return c.json(credential, 201);
  });

  app.delete("/:id/environments/:environmentId/credentials/:envName", (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    const envName = c.req.param("envName");
    const index = found.universe.environmentCredentials.findIndex(
      (credential) =>
        credential.environmentId === found.environment.environmentId && credential.envName === envName,
    );
    if (index < 0) return notFound(c);
    const [credential] = found.universe.environmentCredentials.splice(index, 1);
    return c.json(credential);
  });

  return app;
}

/// Counts derive from environment rows, exactly like the core's view.
function withUsage(universe: UniverseState, key: EnvironmentRegistrationKey): EnvironmentRegistrationKey {
  const mine = [...universe.environments.values()].filter((environment) =>
    environment.source.type === "registered"
    && environment.source.registrationKeyId === key.registrationKeyId);
  const lastRegisteredAtMs = mine.reduce<number | null>(
    (latest, environment) => (latest === null ? environment.createdAtMs : Math.max(latest, environment.createdAtMs)),
    null,
  );
  return {
    ...key,
    registeredEnvironmentCount: mine.length,
    activeEnvironmentCount: mine.filter((environment) => environment.state !== "closed").length,
    ...(lastRegisteredAtMs === null ? {} : { lastRegisteredAtMs }),
  };
}
