/// Universe MCP server catalog with the engine's put-with-expected-revision
/// semantics, advisory auth discovery, and an OAuth flow that approves
/// itself so the sign-in dialog's polling path runs end to end.
import { Hono } from "hono";
import type { McpOAuthFlow, McpServer, SecretGrant } from "@/api";
import type { DemoStore, UniverseState } from "../store";
import { badRequest, conflict, notFound, readBody, universeFor } from "./common";

const APPROVALS = new Set<string>(["always", "never"]);
const STATUSES = new Set<string>(["active", "needsAuthConfig", "unverified", "disabled"]);
const OAUTH_POLICIES = new Set<string>(["optionalOAuth", "requiredOAuth"]);
const REQUIRED_POLICIES = new Set<string>(["requiredBearer", "requiredOAuth"]);
const FLOW_TTL_MS = 10 * 60_000;
const APPROVAL_DELAY_MS = 2_500;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringOrNull(value: unknown): string | null {
  return typeof value === "string" && value.trim() ? value.trim() : null;
}

function stringList(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((entry): entry is string => typeof entry === "string") : [];
}

/// Builds the stored record from a client document, applying the engine's
/// defaults and refusing the combinations it refuses.
function materialize(
  universe: UniverseState,
  input: Record<string, unknown>,
  existing: McpServer | undefined,
): { server: McpServer } | { error: string; status: 400 | 404 } {
  const serverId = typeof input.serverId === "string" ? input.serverId.trim() : "";
  const serverUrl = typeof input.serverUrl === "string" ? input.serverUrl.trim() : "";
  const defaultServerLabel = typeof input.defaultServerLabel === "string"
    ? input.defaultServerLabel.trim()
    : "";
  if (!serverId || !serverUrl || !defaultServerLabel) {
    return { error: "serverId, serverUrl, and defaultServerLabel are required", status: 400 };
  }
  const policyInput = isRecord(input.authPolicy) ? input.authPolicy : null;
  const policyType = policyInput && typeof policyInput.type === "string" ? policyInput.type : "none";
  const authPolicy: McpServer["authPolicy"] = { ...(policyInput ?? {}), type: policyType };
  const credential = isRecord(input.credential) && typeof input.credential.grantId === "string"
    ? { type: "authGrant" as const, grantId: input.credential.grantId }
    : null;
  const status = typeof input.status === "string" && STATUSES.has(input.status)
    ? (input.status as McpServer["status"])
    : "active";
  if (policyType === "none" && credential) {
    return { error: "MCP servers with auth policy none cannot bind a credential", status: 400 };
  }
  if (REQUIRED_POLICIES.has(policyType) && status === "active" && !credential) {
    return { error: "an active MCP server with a required auth policy needs a credential", status: 400 };
  }
  if (credential && !universe.secrets.grants.some((grant) => grant.grantId === credential.grantId)) {
    return { error: `auth grant not found: ${credential.grantId}`, status: 404 };
  }
  const now = Date.now();
  const allowedTools = stringList(input.allowedTools);
  return {
    server: {
      serverId,
      displayName: stringOrNull(input.displayName),
      serverUrl,
      defaultServerLabel,
      description: stringOrNull(input.description),
      allowedTools: allowedTools.length > 0 ? allowedTools : null,
      execution: input.execution === "native" ? "native" : "provider",
      exposure: input.execution === "native" && input.exposure === "search" ? "search" : "inject",
      approval: typeof input.approval === "string" && APPROVALS.has(input.approval)
        ? (input.approval as McpServer["approval"])
        : "never",
      deferLoading: typeof input.deferLoading === "boolean"
        ? input.deferLoading
        : null,
      allowPrivateNetwork: input.allowPrivateNetwork === true,
      authPolicy,
      credential,
      status,
      revision: existing ? existing.revision + 1 : 1,
      createdAtMs: existing?.createdAtMs ?? now,
      updatedAtMs: now,
    },
  };
}

function flowCompletionError(flow: McpOAuthFlow): string | null {
  switch (flow.status) {
    case "completed":
      return flow.grantId ? null : "completed OAuth flow returned no access grant";
    case "pending":
      return "OAuth authorization is still pending";
    case "failed":
      return flow.error ? `OAuth authorization failed: ${flow.error}` : "OAuth authorization failed";
    case "expired":
      return "OAuth authorization expired; start a new login";
  }
}

/// The grant a completed flow leaves behind: brokered, refreshable, owned by
/// the signed-in user like a real MCP OAuth grant.
function mintOAuthGrant(
  store: DemoStore,
  universe: UniverseState,
  server: McpServer,
  scopes: string[],
  audience: string,
): SecretGrant {
  const now = Date.now();
  const grant: SecretGrant = {
    grantId: store.nextId("authgrant"),
    providerId: `mcp:${server.serverId}`,
    providerKind: "mcpOAuth",
    displayName: `${server.displayName ?? server.serverId} · ${store.currentUser.name}`,
    subjectHint: store.currentUser.email,
    status: "active",
    exposure: "brokered",
    createdBy: { kind: "actor", id: store.currentUser.id },
    scopes,
    audience,
    hasAccessToken: true,
    hasRefreshToken: true,
    expiresAtMs: now + 3_600_000,
    lastLeasedAtMs: null,
    leaseCount: 0,
    metadata: {},
    createdAtMs: now,
    updatedAtMs: now,
  };
  universe.secrets.grants.push(grant);
  return grant;
}

export function mcpRoutes(store: DemoStore): Hono {
  const app = new Hono();

  app.get("/:id/mcp-servers", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    return c.json([...universe.mcpServers.values()]);
  });

  app.post("/:id/mcp-servers/:serverId/tools/discover", (c) => {
    const universe = universeFor(store, c);
    const server = universe?.mcpServers.get(c.req.param("serverId"));
    if (!universe || !server) return notFound(c);
    if (server.status === "needsAuthConfig" ||
        (REQUIRED_POLICIES.has(server.authPolicy.type) && !server.credential)) {
      return c.json({
        status: "failure" as const,
        code: "credentialAbsent" as const,
        message: "This MCP server needs a credential before its tools can be discovered",
      });
    }
    return c.json({
      status: "success" as const,
      tools: [
        {
          name: "search",
          title: "Search",
          description: `Search ${server.displayName ?? server.serverId}`,
          annotations: { readOnlyHint: true, openWorldHint: true },
        },
        {
          name: "read",
          title: "Read item",
          description: "Read one item by identifier",
          annotations: { readOnlyHint: true, idempotentHint: true },
        },
        {
          name: "update",
          title: "Update item",
          description: "Update an existing item",
          annotations: { destructiveHint: true },
        },
      ],
    });
  });

  /// Advisory only: https servers that look like protected MCP endpoints
  /// count as OAuth-protected, everything else stays for the user to decide.
  app.post("/:id/mcp-servers/discover-auth", async (c) => {
    if (!universeFor(store, c)) return notFound(c);
    const body = await readBody<{ serverUrl?: unknown }>(c);
    let url: URL;
    try {
      url = new URL(typeof body.serverUrl === "string" ? body.serverUrl : "");
    } catch {
      return badRequest(c, "serverUrl must be a valid URL");
    }
    const protectedLooking = url.protocol === "https:" && /oauth|mcp/i.test(url.href);
    return c.json(
      protectedLooking
        ? {
            oauth: {
              resource: url.href,
              authorizationServers: [url.origin],
              scopesSupported: ["mcp:tools"],
            },
          }
        : { oauth: null },
    );
  });

  app.post("/:id/mcp-servers", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const body = await readBody<Record<string, unknown>>(c);
    const serverId = typeof body.serverId === "string" ? body.serverId.trim() : "";
    if (serverId && universe.mcpServers.has(serverId)) {
      return conflict(c, `MCP server ${serverId} already exists`);
    }
    const result = materialize(universe, body, undefined);
    if ("error" in result) return c.json({ error: result.error }, result.status);
    universe.mcpServers.set(result.server.serverId, result.server);
    return c.json(result.server, 201);
  });

  /// Create-or-replace; a `revision` from GET is the CAS guard.
  app.put("/:id/mcp-servers/:serverId", async (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    const serverId = c.req.param("serverId");
    const body = await readBody<Record<string, unknown>>(c);
    if (body.serverId !== serverId) return badRequest(c, "serverId in document does not match URL");
    const existing = universe.mcpServers.get(serverId);
    if (existing && typeof body.revision === "number" && body.revision !== existing.revision) {
      return conflict(c, `expected revision ${body.revision}, got ${existing.revision}`);
    }
    const result = materialize(universe, body, existing);
    if ("error" in result) return c.json({ error: result.error }, result.status);
    universe.mcpServers.set(serverId, result.server);
    return c.json(result.server);
  });

  app.delete("/:id/mcp-servers/:serverId", (c) => {
    const universe = universeFor(store, c);
    if (!universe) return notFound(c);
    if (!universe.mcpServers.delete(c.req.param("serverId"))) return notFound(c);
    return c.json({ ok: true });
  });

  /// Nobody signs in anywhere: the "provider" approves after a beat, which
  /// is enough for the dialog to poll the flow to completion.
  app.post("/:id/mcp-servers/:serverId/oauth/start", async (c) => {
    const universe = universeFor(store, c);
    const server = universe?.mcpServers.get(c.req.param("serverId"));
    if (!universe || !server) return notFound(c);
    const body = await readBody<{ scopes?: unknown; audience?: unknown }>(c);
    const now = Date.now();
    const flow: McpOAuthFlow = {
      flowId: store.nextId("authflow"),
      clientId: `mcp:${server.serverId}`,
      providerId: `mcp:${server.serverId}`,
      status: "pending",
      grantId: null,
      error: null,
      expiresAtMs: now + FLOW_TTL_MS,
      createdAtMs: now,
      updatedAtMs: now,
    };
    universe.oauthFlows.set(flow.flowId, flow);
    const scopes = Array.isArray(body.scopes)
      ? stringList(body.scopes)
      : stringList(server.authPolicy.scopesDefault);
    const audience = typeof body.audience === "string"
      ? body.audience
      : typeof server.authPolicy.resource === "string"
        ? server.authPolicy.resource
        : server.serverUrl;
    setTimeout(() => {
      if (flow.status !== "pending") return;
      const grant = mintOAuthGrant(store, universe, server, scopes, audience);
      flow.status = "completed";
      flow.grantId = grant.grantId;
      flow.updatedAtMs = Date.now();
    }, APPROVAL_DELAY_MS);
    return c.json(
      {
        flowId: flow.flowId,
        authorizeUrl: "#demo-oauth",
        expiresAtMs: flow.expiresAtMs,
        serverRevision: server.revision,
      },
      201,
    );
  });

  app.get("/:id/mcp-servers/:serverId/oauth/flows/:flowId", (c) => {
    const universe = universeFor(store, c);
    const flow = universe?.oauthFlows.get(c.req.param("flowId"));
    if (!universe || !flow) return notFound(c);
    if (flow.status === "pending" && flow.expiresAtMs <= Date.now()) {
      flow.status = "expired";
      flow.updatedAtMs = Date.now();
    }
    return c.json(flow);
  });

  /// Bind the minted grant at the revision captured when login started; a
  /// concurrent edit conflicts instead of being overwritten.
  app.post("/:id/mcp-servers/:serverId/oauth/flows/:flowId/complete", async (c) => {
    const universe = universeFor(store, c);
    const server = universe?.mcpServers.get(c.req.param("serverId"));
    const flow = universe?.oauthFlows.get(c.req.param("flowId"));
    if (!universe || !server || !flow) return notFound(c);
    const body = await readBody<{ expectedRevision?: unknown }>(c);
    if (typeof body.expectedRevision !== "number") return badRequest(c, "expectedRevision is required");
    const flowError = flowCompletionError(flow);
    const grantId = flow.grantId;
    if (flowError || !grantId) return conflict(c, flowError ?? "completed OAuth flow returned no access grant");
    if (!OAUTH_POLICIES.has(server.authPolicy.type)) return conflict(c, "MCP server does not use OAuth");
    if (server.credential?.grantId === grantId) return c.json(server);
    if (server.revision !== body.expectedRevision) {
      return conflict(
        c,
        "MCP server changed while OAuth authorization was in progress; retry the login against the latest server configuration",
      );
    }
    const updated: McpServer = {
      ...server,
      credential: { type: "authGrant", grantId },
      status: server.status === "needsAuthConfig" ? "active" : server.status,
      revision: server.revision + 1,
      updatedAtMs: Date.now(),
    };
    universe.mcpServers.set(server.serverId, updated);
    return c.json(updated);
  });

  return app;
}
