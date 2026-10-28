/// Deployment-scoped administration: deployment environment providers and
/// their per-universe bindings, the operator channel-account listing, and
/// connector health.
import { Hono } from "hono";
import type { EnvironmentProviderBinding, EnvironmentTemplate } from "@/api";
import type {
  DeploymentApiKeyView,
  DeploymentChannelAccountView,
  DeploymentEnvironmentProviderView,
  MethodGroup,
} from "@lightspeed-ai/sdk";
import { groupsFor } from "@/lib/method-groups";
import { DEMO_POWER_STATES } from "../fixtures/builders";
import type { DemoStore, UniverseState } from "../store";
import { badRequest, conflict, notFound, readBody } from "./common";

/// What a fresh binding provisions from when no other universe already
/// lists this provider's templates.
const DEFAULT_TEMPLATES: Array<Omit<EnvironmentTemplate, "providerId">> = [
  {
    templateId: "dev-small-v1",
    displayName: "Development VM (small)",
    description: "2 vCPU / 4 GiB, Git, Docker, common toolchains, envd.",
    publicIngress: true,
    deprecated: false,
    powerStates: DEMO_POWER_STATES,
    metadata: { cpu: "2", memory: "4GiB", disk: "40GiB" },
  },
  {
    templateId: "dev-large-v1",
    displayName: "Development VM (large)",
    description: "8 vCPU / 16 GiB, same image as small.",
    publicIngress: true,
    deprecated: false,
    powerStates: DEMO_POWER_STATES,
    metadata: { cpu: "8", memory: "16GiB", disk: "120GiB" },
  },
];

function optionalString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

/// The controller URL schemes the engine accepts.
const CONTROLLER_ENDPOINT = /^(wss?:\/\/|unix:\/\/\/)[^\s]+$/;
const DATA_ROUTE_BASE = /^wss?:\/\/[^\s]+$/;

/// The base the core derives from a ws:// or wss:// controller endpoint.
function derivedDataRouteBase(controllerEndpoint: string): string | null {
  const match = /^(wss?:\/\/[^/\s]+)/.exec(controllerEndpoint);
  return match ? `${match[1]}/routes` : null;
}

/// The admin page addresses universes by their engine id; the platform id
/// resolves too so either spelling works.
function universeByAnyId(store: DemoStore, id: string): UniverseState | null {
  const byPlatformId = store.universe(id);
  if (byPlatformId) return byPlatformId;
  for (const state of store.universes.values()) {
    if (state.universe.lightspeedUniverseId === id) return state;
  }
  return null;
}

/// The engine asks the provider for templates per binding; the demo copies
/// what the provider offers elsewhere (or the default set) so a newly bound
/// universe can provision right away.
function seedTemplates(store: DemoStore, universe: UniverseState, binding: EnvironmentProviderBinding): void {
  let source: Array<Omit<EnvironmentTemplate, "providerId">> = [];
  for (const other of store.universes.values()) {
    source = other.environmentTemplates.filter((template) => template.providerId === binding.providerId);
    if (source.length > 0) break;
  }
  for (const template of source.length > 0 ? source : DEFAULT_TEMPLATES) {
    universe.environmentTemplates.push({ ...template, providerId: binding.providerId });
  }
}

export function adminRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.get("/admin/environment-providers", (c) => c.json([...store.environmentProviders.values()]));

  /// Every key: deployment keys, and each universe's keys.
  app.get("/admin/api-keys", (c) => {
    const universeKeys = [...store.universes.values()].flatMap((state) => state.apiKeys);
    return c.json([...store.deploymentKeys, ...universeKeys]);
  });

  app.post("/admin/api-keys", async (c) => {
    const body = await readBody<{
      displayName?: string;
      scope?: { kind: "deployment" } | { kind: "universe"; universeId: string };
      groups?: MethodGroup[];
      assertActor?: boolean;
    }>(c);
    const displayName = body.displayName?.trim();
    if (!displayName || !body.scope) return badRequest(c, "displayName and scope are required");
    const secret = `lsk_${crypto.randomUUID().replaceAll("-", "")}`;
    const key = { keyPrefix: secret.slice(0, 12), displayName, createdAtMs: Date.now(), revokedAtMs: null, lastUsedAtMs: null };
    const kind = body.scope.kind;
    const groups = body.groups ?? groupsFor(kind);
    const assertActor = body.assertActor ?? false;
    if (body.scope.kind === "universe") {
      const universeId = body.scope.universeId;
      const state = store.universes.get(universeId);
      if (!state) return notFound(c, "universe not found");
      const apiKey: DeploymentApiKeyView = {
        ...key, scope: { kind: "universe", universeId: state.universe.lightspeedUniverseId }, groups, assertActor,
        createdBy: { kind: "actor", id: store.currentUser.id },
      };
      state.apiKeys.push(apiKey);
      return c.json({ apiKey, secret }, 201);
    }
    const apiKey: DeploymentApiKeyView = { ...key, scope: { kind: "deployment" }, groups, assertActor, createdBy: { kind: "actor", id: store.currentUser.id } };
    store.deploymentKeys.push(apiKey);
    return c.json({ apiKey, secret }, 201);
  });

  app.post("/admin/api-keys/:keyPrefix/rotate", (c) => {
    const prefix = c.req.param("keyPrefix");
    const apiKey = [...store.deploymentKeys, ...[...store.universes.values()].flatMap((state) => state.apiKeys)]
      .find((key) => key.keyPrefix === prefix && key.revokedAtMs == null);
    if (!apiKey) return notFound(c);
    const secret = `lsk_${crypto.randomUUID().replaceAll("-", "")}`;
    apiKey.keyPrefix = secret.slice(0, 12);
    apiKey.lastUsedAtMs = null;
    return c.json({ apiKey, secret });
  });

  app.delete("/admin/api-keys/:keyPrefix", (c) => {
    const prefix = c.req.param("keyPrefix");
    const key = store.deploymentKeys.find((candidate) => candidate.keyPrefix === prefix)
      ?? [...store.universes.values()].flatMap((state) => state.apiKeys).find((candidate) => candidate.keyPrefix === prefix);
    if (!key) return notFound(c);
    key.revokedAtMs ??= Date.now();
    return c.json(key);
  });

  app.put("/admin/environment-providers/:providerId", async (c) => {
    const providerId = c.req.param("providerId").trim();
    if (!providerId) return badRequest(c, "providerId is required");
    const body = await readBody<{
      displayName?: unknown;
      controllerEndpoint?: unknown;
      dataRouteBase?: unknown;
      credential?: unknown;
      clearCredential?: unknown;
    }>(c);
    const controllerEndpoint = optionalString(body.controllerEndpoint);
    if (!controllerEndpoint || !CONTROLLER_ENDPOINT.test(controllerEndpoint)) {
      return badRequest(c, "controllerEndpoint must be a ws://, wss:// or unix:// URL");
    }
    const dataRouteBase = optionalString(body.dataRouteBase);
    if (dataRouteBase && !DATA_ROUTE_BASE.test(dataRouteBase)) {
      return badRequest(c, "dataRouteBase must be a ws:// or wss:// URL");
    }
    if (!dataRouteBase && controllerEndpoint.startsWith("unix://")) {
      return badRequest(c, "a unix:// controller needs a dataRouteBase");
    }
    const credential = optionalString(body.credential);
    if (credential && body.clearCredential === true) {
      return badRequest(c, "credential and clearCredential are mutually exclusive");
    }
    const existing = store.environmentProviders.get(providerId);
    const now = Date.now();
    const effectiveDataRouteBase = dataRouteBase ?? derivedDataRouteBase(controllerEndpoint);
    const provider: DeploymentEnvironmentProviderView = {
      providerId,
      ...(optionalString(body.displayName) ? { displayName: optionalString(body.displayName) } : {}),
      controllerEndpoint,
      ...(dataRouteBase ? { dataRouteBase } : {}),
      ...(effectiveDataRouteBase ? { effectiveDataRouteBase } : {}),
      hasCredential: credential ? true : body.clearCredential === true ? false : existing?.hasCredential ?? false,
      createdAtMs: existing?.createdAtMs ?? now,
      updatedAtMs: now,
    };
    store.environmentProviders.set(providerId, provider);
    return c.json(provider);
  });

  app.delete("/admin/environment-providers/:providerId", (c) => {
    const providerId = c.req.param("providerId");
    const provider = store.environmentProviders.get(providerId);
    if (!provider) return notFound(c);
    for (const universe of store.universes.values()) {
      if (universe.providerBindings.some((binding) => binding.providerId === providerId)) {
        return conflict(c, "environment provider is referenced by a universe binding");
      }
    }
    store.environmentProviders.delete(providerId);
    return c.json(provider);
  });

  /// Every platform universe with the bindings the engine reports for it.
  app.get("/admin/environment-provider-bindings", (c) =>
    c.json(
      [...store.universes.values()].map((state) => ({
        universeId: state.universe.id,
        lightspeedUniverseId: state.universe.lightspeedUniverseId,
        name: state.universe.name,
        status: state.universe.status,
        bindings: state.providerBindings,
        error: null,
      })),
    ),
  );

  app.put("/admin/universes/:universeId/environment-provider-bindings/:providerId", async (c) => {
    const universe = universeByAnyId(store, c.req.param("universeId"));
    if (!universe) return notFound(c);
    const body = await readBody<{ enabled?: unknown }>(c);
    if (typeof body.enabled !== "boolean") return badRequest(c, "enabled must be a boolean");
    const providerId = c.req.param("providerId");
    if (!store.environmentProviders.has(providerId)) {
      return notFound(c, `environment provider not found: ${providerId}`);
    }
    const index = universe.providerBindings.findIndex((binding) => binding.providerId === providerId);
    const existing = universe.providerBindings[index];
    const now = Date.now();
    const binding: EnvironmentProviderBinding = {
      providerId,
      enabled: body.enabled,
      createdAtMs: existing?.createdAtMs ?? now,
      updatedAtMs: now,
    };
    if (existing) {
      universe.providerBindings[index] = binding;
    } else {
      universe.providerBindings.push(binding);
      seedTemplates(store, universe, binding);
    }
    return c.json(binding);
  });

  app.delete("/admin/universes/:universeId/environment-provider-bindings/:providerId", (c) => {
    const universe = universeByAnyId(store, c.req.param("universeId"));
    if (!universe) return notFound(c);
    const providerId = c.req.param("providerId");
    const index = universe.providerBindings.findIndex((binding) => binding.providerId === providerId);
    if (index < 0) return notFound(c);
    const open = [...universe.environments.values()].some((environment) =>
      environment.source.type === "provisioned"
      && environment.source.providerId === providerId
      && environment.state !== "closed");
    if (open) return conflict(c, "the universe still has open environments of this provider");
    const [binding] = universe.providerBindings.splice(index, 1);
    universe.environmentTemplates = universe.environmentTemplates.filter(
      (template) => template.providerId !== providerId,
    );
    return c.json(binding);
  });

  /// Deployment-wide listing from the core's deployment scope: every account
  /// across universes, each row carrying its universe id. The demo uses the
  /// platform universe id as the core `universeId`.
  app.get("/channel-accounts", (c) => {
    const accounts: DeploymentChannelAccountView[] = [];
    for (const state of store.universes.values()) {
      for (const account of state.channelAccounts.values()) {
        accounts.push({ ...account, universeId: state.universe.id });
      }
    }
    return c.json({ accounts });
  });

  app.get("/status/channels", (c) => c.json(store.channelsStatus));

  return app;
}
