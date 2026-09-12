/// Universe environments: provisioned and external records, the provider
/// bindings and templates they come from, credential bindings, and a
/// simulated provider lifecycle (provisioning → booting → ready, closing →
/// closed, power convergence) so the Environments page has state to watch.
import { Hono } from "hono";
import type { Context } from "hono";
import type {
  Environment,
  EnvironmentCredential,
  EnvironmentCredentialSource,
  EnvironmentRegistrationKey,
} from "@/api";
import type { DemoStore, UniverseState } from "../store";
import { badRequest, conflict, notFound, readBody, universeFor } from "./common";

type PowerState = Environment["desiredPower"];
type LifecycleStatus = Environment["status"];
type IdlePolicy = NonNullable<Environment["idlePolicy"]>;

/// What the demo provider advertises once a target is observed — the Incus
/// provider's set, which has no suspend.
const DEMO_POWER_STATES: PowerState[] = ["running", "paused", "stopped"];

/// The observed status a converged power intent settles on.
const POWER_STATUS: Record<PowerState, LifecycleStatus> = {
  running: "ready",
  paused: "paused",
  suspended: "suspended",
  stopped: "offline",
};

const IDLE_STAGES = ["pauseAfterMs", "suspendAfterMs", "stopAfterMs", "closeAfterMs"] as const;
const ENV_NAME = /^[A-Za-z_][A-Za-z0-9_]{0,127}$/;
const WS_ENDPOINT = /^wss?:\/\/[^\s]+$/;
const DEV_ENVD_ENDPOINT = "ws://127.0.0.1:19091";

export interface ProvisionParams {
  requestId: string;
  bindingId: string;
  templateId: string;
  displayName?: string | null;
  idlePolicy?: unknown;
  metadata?: Record<string, unknown>;
}

/// Pending simulated transitions per environment. A newer intent (close, a
/// power change) bumps the epoch so stale timers drop instead of overwriting
/// what the newer intent already settled.
const epochs = new WeakMap<Environment, number>();

function transition(environment: Environment, steps: Array<[LifecycleStatus, number]>): void {
  const epoch = (epochs.get(environment) ?? 0) + 1;
  epochs.set(environment, epoch);
  for (const [status, delayMs] of steps) {
    setTimeout(() => {
      if (epochs.get(environment) !== epoch || environment.status === "closed") return;
      const now = Date.now();
      environment.status = status;
      environment.updatedAtMs = now;
      environment.incarnation.updatedAtMs = now;
      if (status === "booting" && !environment.incarnation.providerTargetId) {
        environment.incarnation.providerTargetId = `ls-${environment.environmentId}`;
      }
      if (
        status === "ready"
        && environment.source.type === "provisioned"
        && !environment.incarnation.powerStates?.length
      ) {
        environment.incarnation.powerStates = [...DEMO_POWER_STATES];
      }
    }, delayMs);
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isPowerState(value: unknown): value is PowerState {
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

/// The engine's idle-policy rules: at least one stage, every threshold a
/// positive integer, non-decreasing pause → suspend → stop → close.
function parseIdlePolicy(value: unknown): { policy: IdlePolicy | null } | { error: string } {
  if (value === undefined || value === null) return { policy: null };
  if (!isRecord(value)) return { error: "idle policy must be an object" };
  const policy: IdlePolicy = {};
  let previous: [string, number] | null = null;
  for (const key of IDLE_STAGES) {
    const threshold = value[key];
    if (threshold === undefined || threshold === null) continue;
    if (typeof threshold !== "number" || !Number.isInteger(threshold) || threshold <= 0) {
      return { error: `idle policy ${key} must be a positive integer` };
    }
    if (previous && threshold < previous[1]) {
      return { error: `idle policy ${key} must not be below ${previous[0]}` };
    }
    policy[key] = threshold;
    previous = [key, threshold];
  }
  if (!previous) return { error: "idle policy must set at least one stage" };
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
export function provisionEnvironment(
  store: DemoStore,
  universe: UniverseState,
  params: ProvisionParams,
): { environment: Environment; created: boolean } | { error: string } {
  const existing = findByRequestId(universe, params.requestId);
  if (existing) return { environment: existing, created: false };
  const binding = universe.providerBindings.find(
    (candidate) => candidate.bindingId === params.bindingId,
  );
  if (!binding || binding.status !== "enabled") {
    return { error: "environment provider binding is missing or disabled" };
  }
  const idlePolicy = parseIdlePolicy(params.idlePolicy);
  if ("error" in idlePolicy) return { error: idlePolicy.error };
  const template = universe.environmentTemplates.find(
    (candidate) =>
      candidate.templateId === params.templateId
      && (candidate.bindingId === binding.bindingId || candidate.providerId === binding.providerId),
  );
  const environmentId = store.nextId("env");
  const now = Date.now();
  const environment: Environment = {
    environmentId,
    requestId: params.requestId,
    source: { type: "provisioned", providerId: binding.providerId, bindingId: binding.bindingId },
    displayName: params.displayName ?? null,
    status: "provisioning",
    desiredPower: "running",
    ...(idlePolicy.policy ? { idlePolicy: idlePolicy.policy } : {}),
    incarnation: {
      incarnationId: `inc-${environmentId}-1`,
      provisionRequestId: params.requestId,
      templateId: params.templateId,
      createdAtMs: now,
      updatedAtMs: now,
    },
    publicIngressEnabled: false,
    metadata: stringMetadata(params.metadata),
    createdAtMs: now,
    updatedAtMs: now,
  };
  universe.environments.set(environmentId, environment);
  if (template) {
    transition(environment, [["booting", 2_500], ["ready", 6_000]]);
  } else {
    environment.metadata = {
      ...environment.metadata,
      lifecycleError: `unknown template ${params.templateId}`,
    };
    transition(environment, [["failed", 1_500]]);
  }
  return { environment, created: true };
}

/// `environments/close`: closing at once, closed a moment later. Ingress
/// drops immediately because nothing will answer on it.
export function closeEnvironment(universe: UniverseState, environmentId: string): Environment | null {
  const environment = universe.environments.get(environmentId);
  if (!environment) return null;
  if (environment.status === "closed" || environment.status === "closing") return environment;
  environment.status = "closing";
  environment.updatedAtMs = Date.now();
  environment.publicIngressEnabled = false;
  environment.publicEndpoint = null;
  transition(environment, [["closed", 2_000]]);
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

export function environmentRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.get("/:id/environment-provider-bindings", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json(universe.providerBindings);
  });

  /// Templates of enabled bindings only, like the gateway's fan-out.
  app.get("/:id/environment-templates", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const bindingId = c.req.query("bindingId");
    const enabled = new Set(
      universe.providerBindings
        .filter((binding) => binding.status === "enabled")
        .map((binding) => binding.bindingId),
    );
    return c.json(
      universe.environmentTemplates.filter(
        (template) =>
          enabled.has(template.bindingId) && (!bindingId || template.bindingId === bindingId),
      ),
    );
  });

  app.get("/:id/environments", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const providerId = c.req.query("providerId");
    const bindingId = c.req.query("bindingId");
    const status = c.req.query("status");
    const registrationKeyId = c.req.query("registrationKeyId");
    const environments = [...universe.environments.values()].filter((environment) => {
      const source = environment.source;
      return (!providerId || (source.type === "provisioned" && source.providerId === providerId))
        && (!bindingId || (source.type === "provisioned" && source.bindingId === bindingId))
        && (!status || environment.status === status)
        && (!registrationKeyId
          || (source.type === "registered" && source.registrationKeyId === registrationKeyId));
    });
    return c.json(environments);
  });

  app.get("/:id/environment-registration-keys", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json(universe.registrationKeys.map((key) => withUsage(universe, key)));
  });

  app.post("/:id/environment-registration-keys", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{
      displayName?: string;
      identityMode?: "persistent" | "ephemeral";
      maxActiveEnvironments?: number;
      ephemeralDisconnectGraceMs?: number;
      expiresAtMs?: number;
    }>(c);
    const displayName = body.displayName?.trim();
    if (!displayName) return badRequest(c, "displayName is required");
    const now = Date.now();
    const random = crypto.randomUUID().replace(/-/g, "");
    const secret = `lsrk_${random}${crypto.randomUUID().replace(/-/g, "").slice(0, 11)}`;
    const key: EnvironmentRegistrationKey = {
      registrationKeyId: `registration_key_${random}`,
      displayName,
      keyPrefix: secret.slice(0, 12),
      identityMode: body.identityMode === "ephemeral" ? "ephemeral" : "persistent",
      ...(body.maxActiveEnvironments === undefined ? {} : { maxActiveEnvironments: body.maxActiveEnvironments }),
      ephemeralDisconnectGraceMs: body.ephemeralDisconnectGraceMs ?? 300_000,
      ...(body.expiresAtMs === undefined ? {} : { expiresAtMs: body.expiresAtMs }),
      status: "active",
      registeredEnvironmentCount: 0,
      activeEnvironmentCount: 0,
      createdAtMs: now,
    };
    universe.registrationKeys.push(key);
    return c.json({ registrationKey: withUsage(universe, key), secret }, 201);
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
          && !["closing", "closed"].includes(environment.status)
        ) {
          closeEnvironment(universe, environment.environmentId);
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
      bindingId?: unknown;
      templateId?: unknown;
      displayName?: unknown;
      metadata?: unknown;
      idlePolicy?: unknown;
    }>(c);
    const requestId = typeof body.requestId === "string" ? body.requestId.trim() : "";
    const bindingId = typeof body.bindingId === "string" ? body.bindingId.trim() : "";
    const templateId = typeof body.templateId === "string" ? body.templateId.trim() : "";
    if (!requestId || !bindingId || !templateId) {
      return badRequest(c, "requestId, bindingId, and templateId are required");
    }
    const idlePolicy = parseIdlePolicy(body.idlePolicy);
    if ("error" in idlePolicy) return badRequest(c, idlePolicy.error);
    const result = provisionEnvironment(store, universe, {
      requestId,
      bindingId,
      templateId,
      displayName: typeof body.displayName === "string" && body.displayName.trim()
        ? body.displayName.trim()
        : null,
      idlePolicy: idlePolicy.policy,
      metadata: isRecord(body.metadata) ? body.metadata : {},
    });
    if ("error" in result) return conflict(c, result.error);
    return c.json(result.environment, 201);
  });

  /// A directly attached `lightspeed-envd`: no provider, ready at once.
  app.post("/:id/environments/external", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<{ endpoint?: unknown; displayName?: unknown }>(c);
    const endpoint = typeof body.endpoint === "string" ? body.endpoint.trim() : "";
    if (!WS_ENDPOINT.test(endpoint)) return badRequest(c, "endpoint must be a ws:// or wss:// URL");
    const requestId = externalRequestId(endpoint);
    const existing = findByRequestId(universe, requestId);
    if (existing) return c.json(existing, 201);
    const environmentId = store.nextId("env-ext");
    const now = Date.now();
    const environment: Environment = {
      environmentId,
      requestId,
      source: { type: "external", connection: { endpoint, transport: "webSocket" } },
      displayName: typeof body.displayName === "string" && body.displayName.trim()
        ? body.displayName.trim()
        : null,
      status: "ready",
      desiredPower: "running",
      incarnation: { incarnationId: `inc-${environmentId}-1`, createdAtMs: now, updatedAtMs: now },
      publicIngressEnabled: false,
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

  app.delete("/:id/environments/:environmentId", (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    return c.json(closeEnvironment(found.universe, found.environment.environmentId));
  });

  app.put("/:id/environments/:environmentId/power", async (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    const { environment } = found;
    const body = await readBody<{ power?: unknown }>(c);
    const power = body.power;
    if (!isPowerState(power)) {
      return badRequest(c, "power must be one of running, paused, suspended, stopped");
    }
    if (environment.source.type !== "provisioned") {
      return conflict(c, "external environments have no power control");
    }
    if (["closing", "closed", "failed"].includes(environment.status)) {
      return badRequest(c, "cannot change power of a closing, closed, or failed environment");
    }
    const supported = environment.incarnation.powerStates ?? [];
    if (power !== "running" && !supported.includes(power)) {
      return conflict(c, `environment power state ${power} is not supported by the provider`);
    }
    environment.desiredPower = power;
    environment.updatedAtMs = Date.now();
    // The reconciler converges asynchronously; simulate it. Waking goes
    // through booting like a real resume, and an environment still coming
    // up keeps the lifecycle it is already on.
    if (POWER_STATUS[power] !== environment.status) {
      if (power !== "running") {
        transition(environment, [[POWER_STATUS[power], 1_200]]);
      } else if (environment.status !== "provisioning" && environment.status !== "booting") {
        transition(environment, [["booting", 800], ["ready", 2_500]]);
      }
    }
    return c.json(environment);
  });

  app.put("/:id/environments/:environmentId/idle-policy", async (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    const { environment } = found;
    const body = await readBody<{ idlePolicy?: unknown }>(c);
    if (environment.source.type !== "provisioned") {
      return badRequest(c, "external environments have no power control");
    }
    const parsed = parseIdlePolicy(body.idlePolicy);
    if ("error" in parsed) return badRequest(c, parsed.error);
    if (parsed.policy) {
      environment.idlePolicy = parsed.policy;
    } else {
      delete environment.idlePolicy;
    }
    environment.updatedAtMs = Date.now();
    return c.json(environment);
  });

  app.put("/:id/environments/:environmentId/ingress", async (c) => {
    const found = locate(store, c);
    if (!found) return notFound(c);
    const { universe, environment } = found;
    const body = await readBody<{ enabled?: unknown }>(c);
    if (typeof body.enabled !== "boolean") return badRequest(c, "enabled must be a boolean");
    if (environment.source.type !== "provisioned") {
      return conflict(c, "public ingress is available only for provisioned environments");
    }
    const template = universe.environmentTemplates.find(
      (candidate) => candidate.templateId === environment.incarnation.templateId,
    );
    if (body.enabled && !template?.publicIngress) {
      return conflict(c, "provider template does not permit public ingress");
    }
    if (body.enabled && environment.status !== "ready") {
      return conflict(c, "environment must be ready before ingress can be enabled");
    }
    environment.publicIngressEnabled = body.enabled;
    environment.publicEndpoint = body.enabled
      ? `https://${environment.environmentId.replace(/[^a-z0-9]/gi, "")}.env.lightspeed.demo`
      : null;
    environment.updatedAtMs = Date.now();
    return c.json(environment);
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
    activeEnvironmentCount: mine.filter((environment) => environment.status !== "closed").length,
    ...(lastRegisteredAtMs === null ? {} : { lastRegisteredAtMs }),
  };
}
