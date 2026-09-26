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
} from "@lightspeed-ai/agent-client";
import { groupsFor } from "@/lib/method-groups";
import type { DemoStore, UniverseState } from "../store";
import { badRequest, conflict, notFound, readBody } from "./common";

type ControllerConnection = DeploymentEnvironmentProviderView["controllerConnection"];

/// What a fresh binding provisions from when no other universe already
/// lists this provider's templates.
const DEFAULT_TEMPLATES: Array<Omit<EnvironmentTemplate, "providerId" | "bindingId">> = [
  {
    templateId: "dev-small-v1",
    displayName: "Development VM (small)",
    description: "2 vCPU / 4 GiB, Git, Docker, common toolchains, envd.",
    publicIngress: true,
    deprecated: false,
    metadata: { cpu: "2", memory: "4GiB", disk: "40GiB" },
  },
  {
    templateId: "dev-large-v1",
    displayName: "Development VM (large)",
    description: "8 vCPU / 16 GiB, same image as small.",
    publicIngress: true,
    deprecated: false,
    metadata: { cpu: "8", memory: "16GiB", disk: "120GiB" },
  },
];

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function optionalString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value.trim() : undefined;
}

function stringMetadata(value: unknown): Record<string, string> {
  const metadata: Record<string, string> = {};
  if (!isRecord(value)) return metadata;
  for (const [key, entry] of Object.entries(value)) {
    if (typeof entry === "string") metadata[key] = entry;
  }
  return metadata;
}

function parseConnection(value: unknown): ControllerConnection | null {
  if (!isRecord(value)) return null;
  const endpoint = optionalString(value.endpoint);
  const transport = isRecord(value.transport) ? value.transport : null;
  if (!endpoint || !transport) return null;
  switch (transport.type) {
    case "provider": {
      const providerType = optionalString(transport.providerType);
      return providerType ? { endpoint, transport: { type: "provider", providerType } } : null;
    }
    case "webSocket":
    case "http":
    case "stdio":
    case "ssh":
      return { endpoint, transport: { type: transport.type } };
    default:
      return null;
  }
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
  let source: Array<Omit<EnvironmentTemplate, "providerId" | "bindingId">> = [];
  for (const other of store.universes.values()) {
    source = other.environmentTemplates.filter((template) => template.providerId === binding.providerId);
    if (source.length > 0) break;
  }
  for (const template of source.length > 0 ? source : DEFAULT_TEMPLATES) {
    universe.environmentTemplates.push({
      ...template,
      providerId: binding.providerId,
      bindingId: binding.bindingId,
    });
  }
}

export function adminRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.get("/admin/environment-providers", (c) => c.json([...store.environmentProviders.values()]));

  /// Every key: deployment keys, and each universe's keys with the groups
  /// they hold.
  app.get("/admin/api-keys", (c) => {
    const universeKeys = [...store.universes.values()].flatMap((state) => state.apiKeys.map((key): DeploymentApiKeyView => ({
      ...key,
      scope: { kind: "universe", universeId: state.universe.lightspeedUniverseId },
      groups: store.keyGrants.get(key.keyPrefix)?.groups ?? groupsFor("universe"),
      assertActor: store.keyGrants.get(key.keyPrefix)?.assertActor ?? false,
      createdBy: { kind: "actor", id: store.currentUser.id },
    })));
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
      state.apiKeys.push(key);
      store.keyGrants.set(key.keyPrefix, { groups, assertActor });
      const apiKey: DeploymentApiKeyView = {
        ...key, scope: { kind: "universe", universeId: state.universe.lightspeedUniverseId }, groups, assertActor,
        createdBy: { kind: "actor", id: store.currentUser.id },
      };
      return c.json({ apiKey, secret }, 201);
    }
    const apiKey: DeploymentApiKeyView = { ...key, scope: { kind: "deployment" }, groups, assertActor, createdBy: { kind: "actor", id: store.currentUser.id } };
    store.deploymentKeys.push(apiKey);
    return c.json({ apiKey, secret }, 201);
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
    const body = await readBody<{ displayName?: unknown; metadata?: unknown; controllerConnection?: unknown }>(c);
    const controllerConnection = parseConnection(body.controllerConnection);
    if (!controllerConnection) {
      return badRequest(c, "controllerConnection needs an endpoint and a transport");
    }
    const existing = store.environmentProviders.get(providerId);
    const now = Date.now();
    const provider: DeploymentEnvironmentProviderView = {
      providerId,
      ...(optionalString(body.displayName) ? { displayName: optionalString(body.displayName) } : {}),
      controllerConnection,
      metadata: stringMetadata(body.metadata),
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

  app.put("/admin/universes/:universeId/environment-provider-bindings/:bindingId", async (c) => {
    const universe = universeByAnyId(store, c.req.param("universeId"));
    if (!universe) return notFound(c);
    const body = await readBody<{
      providerId?: unknown;
      status?: unknown;
      expectedRevision?: unknown;
      metadata?: unknown;
    }>(c);
    const providerId = optionalString(body.providerId);
    if (!providerId) return badRequest(c, "providerId is required");
    if (body.status !== "enabled" && body.status !== "disabled") {
      return badRequest(c, "status must be enabled or disabled");
    }
    if (!store.environmentProviders.has(providerId)) {
      return notFound(c, `environment provider not found: ${providerId}`);
    }
    const bindingId = c.req.param("bindingId");
    const index = universe.providerBindings.findIndex((binding) => binding.bindingId === bindingId);
    const existing = universe.providerBindings[index];
    if (existing && typeof body.expectedRevision === "number" && existing.revision !== body.expectedRevision) {
      return conflict(c, "binding revision conflict");
    }
    const now = Date.now();
    const binding: EnvironmentProviderBinding = {
      bindingId,
      providerId,
      status: body.status,
      revision: (existing?.revision ?? 0) + 1,
      metadata: stringMetadata(body.metadata),
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

  app.delete("/admin/universes/:universeId/environment-provider-bindings/:bindingId", (c) => {
    const universe = universeByAnyId(store, c.req.param("universeId"));
    if (!universe) return notFound(c);
    const bindingId = c.req.param("bindingId");
    const index = universe.providerBindings.findIndex((binding) => binding.bindingId === bindingId);
    if (index < 0) return notFound(c);
    const [binding] = universe.providerBindings.splice(index, 1);
    universe.environmentTemplates = universe.environmentTemplates.filter(
      (template) => template.bindingId !== bindingId,
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
