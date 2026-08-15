// Stateful stub of the Lightspeed gateway JSON-RPC surface (profiles, VFS
// workspaces, operator/universes) for the universe editing passthrough.
// Lets platform frontend development iterate without the real Rust stack.
//
// Mirrors the current greenfield contract: full-document puts for profiles
// and MCP servers, sparse feature config, universe environment resources.
// Deliberately lenient in one way: universe-scoped calls on universes it
// has never seen still succeed (in-memory state does not survive restarts,
// unlike the real engine's) — operator methods behave strictly.
//
// Records every call at GET /calls; state is in-memory and per-process.
import http from "node:http";
import { randomBytes } from "node:crypto";

const port = Number(process.env.STUB_GATEWAY_PORT ?? 19999);
const calls = [];
const universes = new Map(); // universeId → { createdAtMs }
const workspaces = new Map(); // `${universe}:${workspaceId}` → view
const profiles = new Map(); // `${universe}:${profileId}` → document
const sessions = new Map(); // `${universe}:${sessionId}` → { summary, events }
const provisionedEnvironments = new Map(); // `${universe}:${environmentId}` → environment view
const mcpServers = new Map(); // `${universe}:${serverId}` → view
const apiKeys = new Map(); // `${universe}:${keyPrefix}` → non-secret view
const authGrants = new Map(); // `${universe}:${grantId}` → non-secret view
const authProviders = new Map(); // `${universe}:${providerId}` → non-secret view
const environmentCredentials = new Map(); // `${universe}:${environmentId}:${envName}` → binding view
const demoSeeded = new Set(); // universes with demo sessions
const mcpSeeded = new Set(); // universes with the canned MCP server
const authSeeded = new Set(); // universes with the canned auth grant

/// One canned server per universe so pickers have something to offer.
function ensureDemoMcpServers(universe) {
  if (!universe || mcpSeeded.has(universe)) {
    return;
  }
  mcpSeeded.add(universe);
  mcpServers.set(`${universe}:github`, {
    serverId: "github",
    displayName: "GitHub",
    serverUrl: "https://example.invalid/mcp",
    transport: "auto",
    defaultServerLabel: "github",
    description: null,
    allowedTools: null,
    approvalDefault: "providerDefault",
    deferLoadingDefault: null,
    authPolicy: { type: "none" },
    credential: null,
    status: "active",
    revision: 1,
    createdAtMs: 0,
    updatedAtMs: 0,
  });
}

function ensureDemoAuthGrants(universe) {
  if (!universe || authSeeded.has(universe)) return;
  authSeeded.add(universe);
  authGrants.set(`${universe}:grant-github-demo`, {
    grantId: "grant-github-demo",
    providerId: "github",
    providerKind: "staticBearer",
    principal: { kind: "universeDefault" },
    displayName: "GitHub demo token",
    subjectHint: "octocat",
    scopes: ["repo"],
    hasAccessToken: true,
    hasRefreshToken: false,
    status: "active",
    createdAtMs: 0,
    updatedAtMs: 0,
  });
}
const snapshots = new Map(); // snapshotRef → manifest
const blobs = new Map(); // blobRef → { bytesBase64, bytes }
const DEFAULT_INSTRUCTIONS_REF = "blob-stub-default-instructions";
const defaultInstructionsBytes = Buffer.from("You are a helpful assistant.", "utf8");
blobs.set(DEFAULT_INSTRUCTIONS_REF, {
  bytesBase64: defaultInstructionsBytes.toString("base64"),
  bytes: defaultInstructionsBytes.length,
});
let blobCounter = 0;

const EMPTY_MANIFEST = () => ({
  schema_version: "lightspeed.vfs.snapshot.v1",
  root: { entries: {} },
  totals: { files: 0, bytes: 0 },
});
let snapshotCounter = 0;
let workspaceCounter = 0;
let sessionCounter = 0;
let environmentCounter = 0;
let runCounter = 0;
let itemCounter = 0;
const runsBySubmission = new Map(); // `${universe}:${sessionId}:${submissionId}` → run
let clock = 1;

const prefixed = (map, universe) =>
  [...map.entries()].filter(([key]) => key.startsWith(`${universe}:`)).map(([, v]) => v);

/// Appends one engine-shaped event to a stub session and bumps its
/// updated timestamp.
const pushEvent = (session, kind) => {
  const seq = (session.events.at(-1)?.cursor.seq ?? 0) + 1;
  session.events.push({
    cursor: { seq },
    joins: {},
    kind,
    observedAtMs: Date.now(),
    sessionId: session.summary.id,
  });
  session.summary.updatedAtMs = Date.now();
};

const contextMessage = (id, role, text) => ({
  id,
  contentRef: `blob:${id}`,
  kind: { type: "message", role },
  text,
});

const contextToolCall = (id, callId, name) => ({
  id,
  contentRef: `blob:${id}`,
  display: {
    arguments: '{"path":"notes/dinner.md"}',
    status: "succeeded",
    summary: { group: "explore", verb: "read", target: "notes/dinner.md" },
    toolName: name,
  },
  kind: { type: "toolCall", callId, name },
});

const contextToolResult = (id, callId, output) => ({
  id,
  contentRef: `blob:${id}`,
  kind: { type: "toolResult", callId, isError: false },
  text: output,
});

const contextToolError = (id, callId, output) => ({
  id,
  contentRef: `blob:${id}`,
  kind: { type: "toolResult", callId, isError: true },
  text: output,
});

const contextReasoning = (id, preview) => ({
  id,
  contentRef: `blob:${id}`,
  kind: { type: "reasoningState" },
  preview,
});

const contextToolCallBare = (id, callId, name) => ({
  id,
  contentRef: `blob:${id}`,
  kind: { type: "toolCall", callId, name },
});

const DEFAULT_SESSION_CONFIG = {
  model: {
    providerId: "openai",
    apiKind: "openai:responses",
    model: "gpt-5.4",
  },
};

const sessionRecord = (summary, events = []) => ({
  summary: {
    status: "idle",
    lifecycleStatus: "open",
    config: structuredClone(DEFAULT_SESSION_CONFIG),
    configRevision: 0,
    activeContext: {
      revision: 0,
      entries: [
        {
          id: "stub-default-instructions",
          key: "instructions.000.default",
          kind: { type: "instructions" },
          contentRef: DEFAULT_INSTRUCTIONS_REF,
          mediaType: "text/plain",
          preview: "Default instructions",
        },
      ],
    },
    activeEnvironmentId: null,
    ...summary,
  },
  events,
});

const cannedEnvironments = () => [
  {
    environmentId: "env-local-host",
    providerId: "bridge-local",
    providerTargetId: "local-host",
    origin: "provided",
    status: "ready",
    scope: { type: "default" },
    defaultCwd: "/home/agent",
    capabilities: {
      processStart: true,
      filesystemRead: true,
      filesystemWrite: true,
      jobStart: true,
    },
    connection: {
      endpoint: "ws://127.0.0.1:9000/data/local-host",
      targetId: "local-host",
      transport: { type: "webSocket" },
      defaultCwd: "/home/agent",
      capabilities: {
        processStart: true,
        filesystemRead: true,
        filesystemWrite: true,
        jobStart: true,
      },
    },
    metadata: {},
    observedAtMs: Date.now() - 5_000,
    createdAtMs: 0,
    updatedAtMs: Date.now() - 5_000,
  },
  {
    environmentId: "env-scratch",
    providerId: "bridge-local",
    providerTargetId: "scratch",
    origin: "provided",
    status: "stopped",
    scope: { type: "default" },
    defaultCwd: null,
    capabilities: {},
    connection: {
      endpoint: "ws://127.0.0.1:9000/data/scratch",
      targetId: "scratch",
      transport: { type: "webSocket" },
      defaultCwd: null,
      capabilities: {},
    },
    metadata: {},
    observedAtMs: Date.now() - 5_000,
    createdAtMs: 0,
    updatedAtMs: Date.now() - 5_000,
  },
];

const allEnvironments = (universe) => [
  ...cannedEnvironments(),
  ...prefixed(provisionedEnvironments, universe),
];

const provisionEnvironment = (universe, providerId, request) => {
  const environmentId = `env-provisioned-${++environmentCounter}`;
  const now = Date.now();
  const environment = {
    environmentId,
    providerId,
    providerTargetId: `provisioned-${environmentCounter}`,
    origin: "provisioned",
    status: "ready",
    scope: { type: "default" },
    defaultCwd: request?.spec?.cwd ?? "/workspace",
    capabilities: {
      processStart: true,
      filesystemRead: true,
      filesystemWrite: true,
    },
    connection: {
      endpoint: `ws://127.0.0.1:9000/data/provisioned-${environmentCounter}`,
      targetId: `provisioned-${environmentCounter}`,
      transport: { type: "webSocket" },
      defaultCwd: request?.spec?.cwd ?? "/workspace",
      capabilities: {
        processStart: true,
        filesystemRead: true,
        filesystemWrite: true,
      },
    },
    metadata: {},
    observedAtMs: now,
    createdAtMs: now,
    updatedAtMs: now,
  };
  provisionedEnvironments.set(`${universe}:${environmentId}`, environment);
  return environment;
};

const applyProfileActiveEnvironment = (session, profile) => {
  const environmentId = profile?.activeEnvironmentId;
  if (!environmentId || session.summary.activeEnvironmentId === environmentId) return false;
  session.summary.activeEnvironmentId = environmentId;
  return true;
};

const applyStubProfileInstructions = (session, instructions) => {
  const active = session.summary.activeContext;
  const retained = (active.entries ?? []).filter(
    (entry) => entry.key !== "instructions.000.default" && entry.key !== "instructions.050.profile",
  );
  if (instructions) {
    let contentRef = instructions.blobRef;
    if (instructions.type === "text") {
      const bytes = Buffer.from(instructions.text, "utf8");
      contentRef = `blob-${++blobCounter}`;
      blobs.set(contentRef, { bytesBase64: bytes.toString("base64"), bytes: bytes.length });
    }
    retained.push({
      id: `stub-profile-instructions-${clock++}`,
      key: "instructions.050.profile",
      kind: { type: "instructions" },
      contentRef,
      mediaType: "text/plain",
      preview: "Profile instructions",
    });
  }
  if (!retained.some((entry) => entry.kind?.type === "instructions")) {
    retained.push({
      id: "stub-default-instructions",
      key: "instructions.000.default",
      kind: { type: "instructions" },
      contentRef: DEFAULT_INSTRUCTIONS_REF,
      mediaType: "text/plain",
      preview: "Default instructions",
    });
  }
  const changed = JSON.stringify(active.entries ?? []) !== JSON.stringify(retained);
  if (changed) {
    active.entries = retained;
    active.revision += 1;
    session.summary.updatedAtMs = Date.now();
  }
  return changed;
};

const server = http.createServer((req, res) => {
  if (req.method === "GET" && req.url === "/calls") {
    res.setHeader("content-type", "application/json");
    res.end(JSON.stringify(calls));
    return;
  }
  let body = "";
  req.on("data", (chunk) => (body += chunk));
  req.on("end", () => {
    const { id, method, params = {} } = JSON.parse(body);
    const universe = req.headers["x-lightspeed-universe"] ?? null;
    calls.push({ method, universe });
    const reply = (payload) => {
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ id, ...payload }));
    };
    const ok = (result) => reply({ result: { result } });
    const notFound = () => reply({ error: { code: -32004, message: "not found" } });
    const conflict = (message) => reply({ error: { code: -32009, message } });

    // Operator methods address the deployment: a universe header is a
    // tenant claim that will not be honored — reject it (engine parity).
    if (method.startsWith("operator/") && universe) {
      return reply({
        error: { code: -32602, message: "universe header on operator method" },
      });
    }

    switch (method) {
      case "operator/universes/create": {
        const existing = universes.get(params.universeId);
        if (!existing) {
          universes.set(params.universeId, { createdAtMs: clock++ });
        }
        return ok({
          created: !existing,
          universe: universeView(params.universeId),
        });
      }
      case "operator/universes/list": {
        return ok({ universes: [...universes.keys()].map(universeView) });
      }
      case "operator/universes/read": {
        return universes.has(params.universeId)
          ? ok({ universe: universeView(params.universeId) })
          : notFound();
      }
      case "operator/universes/delete": {
        if (!universes.has(params.universeId)) return notFound();
        universes.delete(params.universeId);
        let blobObjectsDeleted = 0;
        for (const map of [workspaces, profiles, mcpServers, authGrants, authProviders, provisionedEnvironments, environmentCredentials]) {
          for (const key of [...map.keys()]) {
            if (key.startsWith(`${params.universeId}:`)) {
              map.delete(key);
              blobObjectsDeleted += 1;
            }
          }
        }
        for (const key of [...apiKeys.keys()]) {
          if (key.startsWith(`${params.universeId}:`)) apiKeys.delete(key);
        }
        return ok({
          universeId: params.universeId,
          workflowsTerminated: 0,
          blobObjectsDeleted,
        });
      }
      case "operator/api-keys/create": {
        if (!universes.has(params.universeId)) return notFound();
        const secret = `lsk_${randomBytes(32).toString("base64url")}`;
        const keyPrefix = secret.slice(0, 12);
        const apiKey = {
          keyPrefix,
          displayName: params.displayName,
          createdAtMs: Date.now(),
          revokedAtMs: null,
          lastUsedAtMs: null,
        };
        apiKeys.set(`${params.universeId}:${keyPrefix}`, apiKey);
        return ok({ apiKey, secret });
      }
      case "operator/api-keys/list": {
        if (!universes.has(params.universeId)) return notFound();
        return ok({ apiKeys: prefixed(apiKeys, params.universeId) });
      }
      case "operator/api-keys/revoke": {
        if (!universes.has(params.universeId)) return notFound();
        const key = `${params.universeId}:${params.keyPrefix}`;
        const existing = apiKeys.get(key);
        if (!existing) return notFound();
        if (existing.revokedAtMs == null) existing.revokedAtMs = Date.now();
        return ok({ apiKey: existing });
      }

      case "vfs/workspaces/create": {
        const workspaceId = params.workspaceId ?? `ws-${++workspaceCounter}`;
        const key = `${universe}:${workspaceId}`;
        if (workspaces.has(key)) return conflict("workspace exists");
        let snapshotRef = params.snapshotRef;
        if (!snapshotRef) {
          snapshotRef = `snap-${++snapshotCounter}`;
          snapshots.set(snapshotRef, EMPTY_MANIFEST());
        }
        const manifest = snapshots.get(snapshotRef) ?? EMPTY_MANIFEST();
        const ws = {
          workspaceId,
          displayName: params.displayName ?? null,
          headSnapshotRef: snapshotRef,
          baseSnapshotRef: snapshotRef,
          // Engine truth: fresh workspaces start at revision 0 — the stub
          // minting 1 here is exactly how the min(1) file-save bug hid.
          revision: 0,
          files: manifest.totals.files,
          bytes: manifest.totals.bytes,
          createdAtMs: clock,
          updatedAtMs: clock++,
        };
        workspaces.set(key, ws);
        return ok({ workspace: ws });
      }
      case "vfs/workspaces/read": {
        const ws = workspaces.get(`${universe}:${params.workspaceId}`);
        return ws ? ok({ workspace: ws }) : notFound();
      }
      case "vfs/workspaces/update": {
        const key = `${universe}:${params.workspaceId}`;
        const ws = workspaces.get(key);
        if (!ws) return notFound();
        if (
          params.expectedRevision !== undefined &&
          params.expectedRevision !== null &&
          params.expectedRevision !== ws.revision
        ) {
          return conflict("revision conflict");
        }
        const manifest = snapshots.get(params.snapshotRef);
        if (!manifest) return notFound();
        const updated = {
          ...ws,
          headSnapshotRef: params.snapshotRef,
          displayName: params.displayName ?? ws.displayName,
          revision: ws.revision + 1,
          files: manifest.totals.files,
          bytes: manifest.totals.bytes,
          updatedAtMs: clock++,
        };
        workspaces.set(key, updated);
        return ok({ workspace: updated });
      }
      case "vfs/workspaces/list": {
        const list = prefixed(workspaces, universe).sort(
          (a, b) => b.updatedAtMs - a.updatedAtMs,
        );
        return ok({ workspaces: list });
      }
      case "vfs/snapshots/commit": {
        const manifest = params.manifest;
        if (manifest?.schema_version !== "lightspeed.vfs.snapshot.v1") {
          return reply({ error: { code: -32602, message: "bad manifest" } });
        }
        const snapshotRef = `snap-${++snapshotCounter}`;
        snapshots.set(snapshotRef, manifest);
        return ok({
          snapshotRef,
          files: manifest.totals.files,
          bytes: manifest.totals.bytes,
        });
      }
      case "vfs/snapshots/read": {
        const manifest = snapshots.get(params.snapshotRef);
        if (!manifest) return notFound();
        return ok({
          snapshotRef: params.snapshotRef,
          manifest,
          files: manifest.totals.files,
          bytes: manifest.totals.bytes,
        });
      }
      case "blobs/put": {
        const stored = (params.blobs ?? []).map((blob) => {
          const blobRef = `blob-${++blobCounter}`;
          const bytes = Buffer.from(blob.bytesBase64 ?? "", "base64").length;
          blobs.set(blobRef, { bytesBase64: blob.bytesBase64 ?? "", bytes });
          return { blobRef, bytes };
        });
        return ok({ blobs: stored });
      }
      case "blobs/read": {
        const blob = blobs.get(params.blobRef);
        if (!blob) return notFound();
        return ok({ blobRef: params.blobRef, bytes: blob.bytes, bytesBase64: blob.bytesBase64 });
      }

      case "profiles/read": {
        const profile = profiles.get(`${universe}:${params.profileId}`);
        return profile ? ok({ profile }) : notFound();
      }
      case "profiles/list": {
        const summaries = prefixed(profiles, universe).map((p) => ({
          profileId: p.profileId,
          displayName: p.displayName ?? null,
          description: p.description ?? null,
          revision: p.revision,
          updatedAtMs: p.updatedAtMs ?? 0,
        }));
        return ok({ profiles: summaries });
      }
      case "profiles/create": {
        const key = `${universe}:${params.profile.profileId}`;
        if (profiles.has(key)) return conflict("profile exists");
        const profile = {
          ...params.profile,
          revision: 1,
          createdAtMs: clock,
          updatedAtMs: clock++,
        };
        profiles.set(key, profile);
        return ok({ profile });
      }
      case "profiles/put": {
        const key = `${universe}:${params.profile.profileId}`;
        const existing = profiles.get(key);
        if (
          existing &&
          params.expectedRevision !== undefined &&
          params.expectedRevision !== null &&
          params.expectedRevision !== existing.revision
        ) {
          return conflict("revision conflict");
        }
        const profile = {
          ...params.profile,
          revision: existing ? existing.revision + 1 : 1,
          createdAtMs: existing ? existing.createdAtMs : clock,
          updatedAtMs: clock++,
        };
        profiles.set(key, profile);
        return ok({ profile });
      }
      case "profiles/delete": {
        const key = `${universe}:${params.profileId}`;
        const profile = profiles.get(key);
        if (!profile) return notFound();
        profiles.delete(key);
        return ok({ profile });
      }

      case "auth/grants/import": {
        const grantId = params.grantId ?? `authgrant_stub_${clock++}`;
        const key = `${universe}:${grantId}`;
        if (authGrants.has(key)) return conflict("auth grant exists");
        const grant = {
          grantId,
          providerId: params.providerId ?? "static",
          providerKind: "staticBearer",
          principal: { kind: "user", id: "stub-user" },
          displayName: params.displayName ?? null,
          subjectHint: params.subjectHint ?? null,
          scopes: params.scopes ?? [],
          audience: params.audience ?? null,
          hasAccessToken: true,
          hasRefreshToken: false,
          expiresAtMs: params.expiresAtMs ?? null,
          status: "active",
          metadata: {},
          createdAtMs: Date.now(),
          updatedAtMs: Date.now(),
        };
        authGrants.set(key, grant);
        return ok({ grant });
      }
      case "auth/grants/read": {
        const grant = authGrants.get(`${universe}:${params.grantId}`);
        return grant ? ok({ grant }) : notFound();
      }
      case "auth/grants/revoke": {
        const grant = authGrants.get(`${universe}:${params.grantId}`);
        if (!grant) return notFound();
        grant.status = "revoked";
        grant.updatedAtMs = Date.now();
        return ok({ grant });
      }
      case "auth/providers/create": {
        const providerId = params.providerId ?? `provider_stub_${clock++}`;
        const key = `${universe}:${providerId}`;
        if (authProviders.has(key)) return conflict("auth provider exists");
        const provider = {
          providerId,
          providerKind: params.config?.type === "modelOAuth" ? "modelOAuth" : "modelApiKey",
          displayName: params.displayName ?? null,
          config: params.config,
          hasCredential: Boolean(params.credential) || params.config?.type === "modelOAuth",
          status: "active",
          createdAtMs: Date.now(),
          updatedAtMs: Date.now(),
        };
        authProviders.set(key, provider);
        return ok({ provider });
      }
      case "auth/providers/read": {
        const provider = authProviders.get(`${universe}:${params.providerId}`);
        return provider ? ok({ provider }) : notFound();
      }
      case "auth/providers/list":
        return ok({ providers: prefixed(authProviders, universe) });
      case "auth/providers/delete": {
        const key = `${universe}:${params.providerId}`;
        const provider = authProviders.get(key);
        if (!provider) return notFound();
        authProviders.delete(key);
        return ok({ provider });
      }

      case "environments/providers/list": {
        // One canned online provider so the observability page and the
        // profile editor's pickers are exercisable without a real runner.
        return ok({
          providers: [
            {
              providerId: "bridge-local",
              providerKind: "bridge",
              displayName: "Local bridge",
              status: "online",
              lastSeenMs: Date.now() - 5_000,
              leaseExpiresMs: Date.now() + 55_000,
              implementation: { name: "stub-bridge", version: "0.0.1" },
              controllerConnection: {
                endpoint: "ws://127.0.0.1:9000/controller",
                transport: { type: "webSocket" },
              },
              capabilities: { listTargets: true, getTarget: true, createTarget: true, closeTarget: true },
            },
          ],
        });
      }
      case "environments/list": {
        const environments = allEnvironments(universe).filter((environment) =>
          (!params.providerId || environment.providerId === params.providerId)
          && (!params.status || environment.status === params.status));
        return ok({ environments });
      }
      case "environments/create": {
        if (params.providerId !== "bridge-local") return notFound();
        const environment = provisionEnvironment(universe, params.providerId, params.request);
        return ok({ environment });
      }
      case "environments/read": {
        const environment = allEnvironments(universe).find(
          (candidate) => candidate.environmentId === params.environmentId,
        );
        return environment ? ok({ environment }) : notFound();
      }
      case "environments/close": {
        const environment = provisionedEnvironments.get(`${universe}:${params.environmentId}`);
        if (!environment) return notFound();
        environment.status = "closed";
        environment.updatedAtMs = Date.now();
        return ok({ environment });
      }
      case "environments/credentials/list": {
        const environment = allEnvironments(universe).find(
          (candidate) => candidate.environmentId === params.environmentId,
        );
        if (!environment) return notFound();
        const prefix = `${universe}:${params.environmentId}:`;
        const credentials = [...environmentCredentials.entries()]
          .filter(([key]) => key.startsWith(prefix))
          .map(([, credential]) => credential)
          .sort((a, b) => a.envName.localeCompare(b.envName));
        return ok({ credentials });
      }
      case "environments/credentials/bind": {
        const environment = allEnvironments(universe).find(
          (candidate) => candidate.environmentId === params.environmentId,
        );
        if (!environment) return notFound();
        if (params.source.type === "authGrant") {
          const grant = authGrants.get(`${universe}:${params.source.grantId}`);
          if (!grant || grant.status !== "active") return notFound();
        } else if (params.source.type === "authProviderCredential") {
          const provider = authProviders.get(`${universe}:${params.source.providerId}`);
          if (!provider || provider.status !== "active" || !provider.hasCredential) return notFound();
        }
        const key = `${universe}:${params.environmentId}:${params.envName}`;
        const existing = environmentCredentials.get(key);
        const credential = {
          environmentId: params.environmentId,
          envName: params.envName,
          source: params.source,
          createdAtMs: existing?.createdAtMs ?? Date.now(),
          updatedAtMs: Date.now(),
        };
        environmentCredentials.set(key, credential);
        return ok({ credential });
      }
      case "environments/credentials/unbind": {
        const key = `${universe}:${params.environmentId}:${params.envName}`;
        const credential = environmentCredentials.get(key);
        if (!credential) return notFound();
        environmentCredentials.delete(key);
        return ok({ credential });
      }

      case "mcp/servers/list": {
        ensureDemoMcpServers(universe);
        return ok({ servers: prefixed(mcpServers, universe) });
      }
      case "models/list": {
        return ok({
          models: [
            {
              providerId: "openai",
              apiKind: "openai:responses",
              model: "gpt-5.4",
              displayName: "GPT-5.4",
              capabilities: {
                maxInputTokens: 272000,
                maxOutputTokens: 128000,
                parallelToolUse: true,
                reasoningEfforts: ["none", "low", "medium", "high", "xhigh"],
              },
              source: "provider",
              fetchedAtMs: Date.now(),
            },
            {
              providerId: "openai",
              apiKind: "openai:responses",
              model: "gpt-5.4-mini",
              displayName: "GPT-5.4 mini",
              capabilities: {
                maxInputTokens: 272000,
                maxOutputTokens: 128000,
                parallelToolUse: true,
                reasoningEfforts: ["none", "low", "medium", "high"],
              },
              source: "provider",
              fetchedAtMs: Date.now(),
            },
          ],
          providers: [
            {
              providerId: "openai",
              apiKinds: ["openai:responses"],
              fetchedAtMs: Date.now(),
              error: null,
            },
          ],
        });
      }
      case "auth/grants/list": {
        ensureDemoAuthGrants(universe);
        const grants = prefixed(authGrants, universe);
        return ok({
          grants: params.status ? grants.filter((grant) => grant.status === params.status) : grants,
        });
      }
      case "mcp/servers/put": {
        ensureDemoMcpServers(universe);
        const input = params.server ?? {};
        const authPolicy = input.authPolicy ?? { type: "none" };
        const credential = input.credential ?? null;
        const status = input.status ?? "active";
        if (authPolicy.type === "none" && credential) {
          return reply({
            error: {
              code: -32602,
              message: "MCP servers with auth policy none cannot bind a credential",
            },
          });
        }
        if (
          ["requiredBearer", "requiredOAuth"].includes(authPolicy.type)
          && status !== "needsAuthConfig"
          && !credential
        ) {
          return reply({
            error: {
              code: -32010,
              message: "active MCP server requires an auth grant",
            },
          });
        }
        if (credential && !authGrants.has(`${universe}:${credential.grantId}`)) {
          return notFound();
        }
        const key = `${universe}:${input.serverId}`;
        const existing = mcpServers.get(key);
        if (
          existing &&
          params.expectedRevision !== undefined &&
          params.expectedRevision !== null &&
          params.expectedRevision !== existing.revision
        ) {
          return conflict("revision conflict");
        }
        const server = {
          serverId: input.serverId,
          displayName: input.displayName ?? null,
          serverUrl: input.serverUrl,
          transport: input.transport ?? "auto",
          defaultServerLabel: input.defaultServerLabel,
          description: input.description ?? null,
          allowedTools: input.allowedTools ?? null,
          approvalDefault: input.approvalDefault ?? "never",
          deferLoadingDefault: input.deferLoadingDefault ?? null,
          authPolicy,
          credential,
          status,
          revision: existing ? existing.revision + 1 : 1,
          createdAtMs: existing ? existing.createdAtMs : Date.now(),
          updatedAtMs: Date.now(),
        };
        mcpServers.set(key, server);
        return ok({ server });
      }
      case "mcp/servers/read": {
        ensureDemoMcpServers(universe);
        const server = mcpServers.get(`${universe}:${params.serverId}`);
        return server ? ok({ server }) : notFound();
      }
      case "mcp/servers/delete": {
        const key = `${universe}:${params.serverId}`;
        const server = mcpServers.get(key);
        if (!server) return notFound();
        mcpServers.delete(key);
        return ok({ server });
      }
      case "session/list": {
        ensureDemoSessions(universe);
        const list = prefixed(sessions, universe).sort(
          (a, b) => b.summary.updatedAtMs - a.summary.updatedAtMs,
        );
        return ok({ sessions: list.map((s) => s.summary), nextCursor: null });
      }
      case "session/read": {
        ensureDemoSessions(universe);
        const s = sessions.get(`${universe}:${params.sessionId}`);
        return s ? ok({ session: s.summary }) : notFound();
      }
      case "session/close": {
        ensureDemoSessions(universe);
        const s = sessions.get(`${universe}:${params.sessionId}`);
        if (!s) return notFound();
        if (s.summary.status === "active" && !params.force) {
          return conflict("session has active work");
        }
        if (s.summary.status !== "closed") {
          s.summary.status = "closed";
          s.summary.lifecycleStatus = "closed";
          s.summary.updatedAtMs = Date.now();
          pushEvent(s, { type: "sessionClosed" });
        }
        return ok({ session: s.summary });
      }
      case "session/delete": {
        ensureDemoSessions(universe);
        const key = `${universe}:${params.sessionId}`;
        const s = sessions.get(key);
        if (!s) return notFound();
        if (s.summary.lifecycleStatus !== "closed") {
          return conflict("only closed sessions can be deleted");
        }
        sessions.delete(key);
        return ok({ session: s.summary });
      }
      case "session/profiles/apply": {
        ensureDemoSessions(universe);
        const s = sessions.get(`${universe}:${params.sessionId}`);
        if (!s) return notFound();
        if (s.summary.status !== "idle") {
          return conflict("managed instructions can only change while idle");
        }
        const profile = params.profile?.kind === "named"
          ? profiles.get(`${universe}:${params.profile.profileId}`)
          : params.profile?.profile;
        if (params.profile?.kind === "named" && !profile) return notFound();
        const instructionsChanged = applyStubProfileInstructions(s, profile?.instructions);
        let configChanged = false;
        if (profile?.config) {
          const config = {
            ...structuredClone(profile.config),
            model: structuredClone(profile.config.model ?? DEFAULT_SESSION_CONFIG.model),
          };
          configChanged = JSON.stringify(config) !== JSON.stringify(s.summary.config);
          if (configChanged) {
            s.summary.config = config;
            s.summary.configRevision += 1;
          }
        }
        const activeEnvironmentChanged = applyProfileActiveEnvironment(s, profile);
        return ok({
          session: s.summary,
          applied: {
            configChanged,
            instructionsChanged,
            activeEnvironmentChanged,
          },
        });
      }
      case "session/config/put": {
        ensureDemoSessions(universe);
        const s = sessions.get(`${universe}:${params.sessionId}`);
        if (!s) return notFound();
        if (s.summary.status !== "idle") return conflict("session config can only change while idle");
        if (
          params.expectedConfigRevision !== undefined
          && params.expectedConfigRevision !== s.summary.configRevision
        ) {
          return conflict(
            `expected config revision ${params.expectedConfigRevision}, got ${s.summary.configRevision}`,
          );
        }
        const config = {
          ...structuredClone(params.config ?? {}),
          model: structuredClone(params.config?.model ?? DEFAULT_SESSION_CONFIG.model),
        };
        if (config.model.apiKind !== s.summary.config.model.apiKind) {
          return conflict(`session provider api kind is pinned to ${s.summary.config.model.apiKind}`);
        }
        if (JSON.stringify(config) !== JSON.stringify(s.summary.config)) {
          s.summary.config = config;
          s.summary.configRevision += 1;
          s.summary.updatedAtMs = Date.now();
        }
        return ok({ session: s.summary });
      }
      case "session/environments/activate": {
        ensureDemoSessions(universe);
        const s = sessions.get(`${universe}:${params.sessionId}`);
        if (!s) return notFound();
        if (s.summary.status !== "idle") return conflict("environment activation requires an idle session");
        const feature = s.summary.config.features?.environments;
        if (!feature) return conflict("environment activation requires the environments feature");
        const environment = allEnvironments(universe).find(
          (candidate) => candidate.environmentId === params.environmentId,
        );
        if (!environment) return notFound();
        if (feature.providers?.length && !feature.providers.includes(environment.providerId)) {
          return conflict(`environment provider is not allowed: ${environment.providerId}`);
        }
        if (environment.status !== "ready") return conflict("environment is not selectable");
        s.summary.activeEnvironmentId = params.environmentId;
        return ok({ session: s.summary });
      }
      case "session/environments/deactivate": {
        ensureDemoSessions(universe);
        const s = sessions.get(`${universe}:${params.sessionId}`);
        if (!s) return notFound();
        if (s.summary.status !== "idle") return conflict("environment deactivation requires an idle session");
        s.summary.activeEnvironmentId = null;
        return ok({ session: s.summary });
      }
      case "session/events/read": {
        ensureDemoSessions(universe);
        const s = sessions.get(`${universe}:${params.sessionId}`);
        if (!s) return notFound();
        const after = params.after?.seq ?? 0;
        const page = () => {
          const events = s.events.filter((e) => e.cursor.seq > after);
          return {
            events,
            nextCursor: events.at(-1)?.cursor ?? null,
            headCursor: s.events.at(-1)?.cursor ?? null,
            complete: true,
            gap: null,
          };
        };
        const first = page();
        if (first.events.length > 0 || !params.waitMs) return ok(first);
        // Crude long-poll: re-check every 250ms until an event lands or
        // the (capped) wait elapses. Enough for the web tail in dev.
        const deadline = Date.now() + Math.min(params.waitMs, 5000);
        const timer = setInterval(() => {
          const next = page();
          if (next.events.length > 0 || Date.now() >= deadline) {
            clearInterval(timer);
            ok(next);
          }
        }, 250);
        return;
      }

      /// Web "New session": engine mints the id when absent. Response
      /// mirrors session/read's summary-as-view simplification.
      case "session/start": {
        const sessionId = params.sessionId ?? `web-${++sessionCounter}`;
        const key = `${universe}:${sessionId}`;
        if (!sessions.has(key)) {
          const now = Date.now();
          const profile = params.profile?.kind === "named"
            ? profiles.get(`${universe}:${params.profile.profileId}`)
            : params.profile?.profile;
          if (params.profile?.kind === "named" && !profile) return notFound();
          const config = {
            ...structuredClone(profile?.config ?? {}),
            model: structuredClone(profile?.config?.model ?? DEFAULT_SESSION_CONFIG.model),
          };
          sessions.set(key, sessionRecord(
            {
              id: sessionId,
              displayName: params.displayName ?? null,
              createdAtMs: now,
              updatedAtMs: now,
              config,
              activeEnvironmentId: profile?.activeEnvironmentId ?? null,
            },
            [],
          ));
          applyStubProfileInstructions(sessions.get(key), profile?.instructions);
          pushEvent(sessions.get(key), { type: "sessionOpened", model: null });
        }
        return ok({ session: sessions.get(key).summary });
      }

      /// One run per message; a canned assistant reply lands ~1s later so
      /// the web's long-poll tail has something to pick up. submissionId
      /// dedupes retries like the engine does.
      case "session/runs/start": {
        ensureDemoSessions(universe);
        const s = sessions.get(`${universe}:${params.sessionId}`);
        if (!s) return notFound();
        const subKey = `${universe}:${params.sessionId}:${params.submissionId ?? ""}`;
        if (params.submissionId && runsBySubmission.has(subKey)) {
          return ok({ run: runsBySubmission.get(subKey) });
        }
        const runId = `run-${++runCounter}`;
        const run = { id: runId, status: "running", source: params.source };
        if (params.submissionId) runsBySubmission.set(subKey, run);
        const text = (params.source?.items ?? [])
          .filter((item) => item.type === "text")
          .map((item) => item.text)
          .join("\n");
        pushEvent(s, { type: "runAccepted", runId, source: { type: "input" } });
        pushEvent(s, { type: "runStarted", runId });
          pushEvent(s, {
            type: "contextEntriesApplied",
            baseRevision: 0,
            revision: 0,
            entries: [contextMessage(`wi-${++itemCounter}`, "user", text)],
          });
        setTimeout(() => {
          if (run.status !== "running") return;
          const callId = `c-${++itemCounter}`;
          pushEvent(s, {
            type: "contextEntriesApplied",
            baseRevision: 0,
            revision: 0,
            entries: [
              contextToolCall(`wi-${itemCounter}`, callId, "read_file"),
              contextToolResult(
                `wi-${++itemCounter}`,
                callId,
                "Stub read completed successfully.",
              ),
              contextMessage(
                `wi-${++itemCounter}`,
                "assistant",
                `Stub reply to: ${text.slice(0, 120)}\n\nThis is a canned response from the dev stub.`,
              ),
            ],
          });
          pushEvent(s, { type: "runCompleted", runId, outputRef: null });
          run.status = "completed";
        }, 1000);
        return ok({ run });
      }

      case "session/runs/cancel": {
        const s = sessions.get(`${universe}:${params.sessionId}`);
        if (!s) return notFound();
        for (const run of runsBySubmission.values()) {
          if (run.id === params.runId && run.status === "running") {
            run.status = "cancelled";
          }
        }
        pushEvent(s, { type: "runCancelled", runId: params.runId });
        return ok({});
      }

      default:
        return reply({ error: { code: -32601, message: `unknown method ${method}` } });
    }
  });
});

/// Canned sessions per universe so the sessions browser has something
/// real-shaped to render without the Rust stack. Event shapes mirror
/// SessionEventView exactly (cursor.seq, joins, kind, observedAtMs).
function ensureDemoSessions(universe) {
  if (!universe || demoSeeded.has(universe)) {
    return;
  }
  demoSeeded.add(universe);
  const base = Date.now() - 3_600_000;
  const mk = (sessionId, seq, kind, offsetMs) => ({
    cursor: { seq },
    joins: {},
    kind,
    observedAtMs: base + offsetMs,
    sessionId,
  });
  const chat = `stub-${universe.slice(0, 4)}-company`;
  sessions.set(`${universe}:${chat}`, sessionRecord(
    {
      id: chat,
      displayName: "telegram: Company Group",
      createdAtMs: base,
      updatedAtMs: base + 240_000,
    },
    [
      mk(chat, 1, { type: "sessionOpened", model: null }, 0),
      mk(chat, 2, { type: "runAccepted", runId: "run_1", source: { type: "input" } }, 1000),
      mk(chat, 3, { type: "runStarted", runId: "run_1" }, 1100),
      mk(
        chat,
        4,
        {
          type: "contextEntriesApplied",
          baseRevision: 0,
          revision: 1,
          entries: [
            contextMessage("i1", "user", "What's going on in project Y?"),
          ],
        },
        1200,
      ),
      mk(
        chat,
        5,
        {
          type: "contextEntriesApplied",
          baseRevision: 1,
          revision: 2,
          entries: [
            contextToolCall("i2", "c1", "message_send"),
            contextToolResult("i3", "c1", "sent"),
            contextMessage(
              "i4",
              "assistant",
              "Stub reply to: What's going on in project Y?\n\nThis is a canned response from the dev stub.",
            ),
          ],
        },
        60_000,
      ),
      mk(chat, 6, { type: "runCompleted", runId: "run_1", outputRef: null }, 61_000),
    ],
  ));

  const solo = `stub-${universe.slice(0, 4)}-lukas`;
  sessions.set(`${universe}:${solo}`, sessionRecord(
    {
      id: solo,
      displayName: "telegram: Lukas",
      createdAtMs: base + 300_000,
      updatedAtMs: base + 302_000,
    },
    [
      mk(solo, 1, { type: "sessionOpened", model: null }, 300_000),
      mk(
        solo,
        2,
        {
          type: "contextEntriesApplied",
          baseRevision: 0,
          revision: 1,
          entries: [contextMessage("s1", "user", "ping")],
        },
        301_000,
      ),
      mk(
        solo,
        3,
        {
          type: "contextEntriesApplied",
          baseRevision: 1,
          revision: 2,
          entries: [contextMessage("s2", "assistant", "pong")],
        },
        302_000,
      ),
    ],
  ));

  const markdown = `stub-${universe.slice(0, 4)}-markdown`;
  const markdownExample = [
    "# Markdown rendering",
    "",
    "This session exercises **bold text**, *emphasis*, ~~strikethrough~~, and \`inline code\`.",
    "",
    "## Lists",
    "",
    "- A regular list item",
    "- Another item with a [link](https://ui.shadcn.com/)",
    "  - Nested items work too",
    "",
    "1. First step",
    "2. Second step",
    "",
    "- [x] GitHub-flavored task items",
    "- [ ] One thing left to do",
    "",
    "> A blockquote can call attention to an important note without taking over the page.",
    "",
    "## Code",
    "",
    "\`\`\`ts",
    "type Greeting = { message: string };",
    "",
    "const greeting: Greeting = { message: \"Hello from the stub\" };",
    "console.log(greeting.message);",
    "\`\`\`",
    "",
    "## Table",
    "",
    "| Feature | Result |",
    "| --- | --- |",
    "| Headings and lists | Rendered |",
    "| Fenced code | Rendered |",
    "| GFM tables | Rendered |",
    "",
    "---",
    "",
    "That covers the main formatting an assistant response is likely to use.",
  ].join("\n");
  sessions.set(`${universe}:${markdown}`, sessionRecord(
    {
      id: markdown,
      displayName: "Markdown rendering example",
      createdAtMs: base + 600_000,
      updatedAtMs: base + 602_000,
    },
    [
      mk(markdown, 1, { type: "sessionOpened", model: null }, 600_000),
      mk(
        markdown,
        2,
        {
          type: "contextEntriesApplied",
          baseRevision: 0,
          revision: 1,
          entries: [
            contextMessage("md1", "user", "Show me the supported Markdown formatting."),
          ],
        },
        601_000,
      ),
      mk(
        markdown,
        3,
        {
          type: "contextEntriesApplied",
          baseRevision: 1,
          revision: 2,
          entries: [contextMessage("md2", "assistant", markdownExample)],
        },
        602_000,
      ),
    ],
  ));

  const traces = `stub-${universe.slice(0, 4)}-tool-traces`;
  const inspectCalls = [
    {
      callId: "trace-call-search",
      toolName: "exec_command",
      argumentsRef: "blob:trace-search-args",
      arguments: JSON.stringify({
        argv: ["rg", "-n", "MessageScroller|TranscriptEntry", "platform/web/src"],
        timeout_ms: 10_000,
      }),
      display: {
        group: "explore",
        verb: "Search",
        target: "MessageScroller|TranscriptEntry in platform/web/src",
        detail: "42 matches",
      },
    },
    {
      callId: "trace-call-read",
      toolName: "read_file",
      argumentsRef: "blob:trace-read-args",
      arguments: JSON.stringify({ path: "platform/web/src/lib/sessions/transcript.ts" }),
      display: {
        group: "explore",
        verb: "Read",
        target: "platform/web/src/lib/sessions/transcript.ts",
      },
    },
    {
      callId: "trace-call-status",
      toolName: "exec_command",
      argumentsRef: "blob:trace-status-args",
      arguments: JSON.stringify({ argv: ["git", "status", "--short"] }),
      display: {
        group: "execute",
        verb: "Run",
        target: "git status --short",
      },
    },
  ];
  const failedCall = {
    callId: "trace-call-health",
    toolName: "web_fetch",
    argumentsRef: "blob:trace-health-args",
    arguments: JSON.stringify({ url: "https://example.invalid/health" }),
    display: {
      group: "explore",
      verb: "Fetch",
      target: "https://example.invalid/health",
      detail: "deployment health",
    },
  };
  sessions.set(`${universe}:${traces}`, sessionRecord(
    {
      id: traces,
      displayName: "Tool calls & thinking example",
      createdAtMs: base + 900_000,
      updatedAtMs: base + 938_000,
    },
    [
      mk(traces, 1, { type: "sessionOpened", model: null }, 900_000),
      mk(traces, 2, { type: "runAccepted", runId: "trace-run", source: { type: "input" } }, 901_000),
      mk(traces, 3, { type: "runStarted", runId: "trace-run" }, 902_000),
      mk(traces, 4, {
        type: "contextEntriesApplied",
        baseRevision: 0,
        revision: 1,
        entries: [contextMessage("trace-user", "user", "Investigate why session activity is hard to follow in the web UI.")],
      }, 903_000),
      mk(traces, 5, { type: "turnStarted", runId: "trace-run", turnId: "trace-turn-1" }, 904_000),
      mk(traces, 6, { type: "turnPlanned", runId: "trace-run", turnId: "trace-turn-1" }, 905_000),
      mk(traces, 7, { type: "turnGenerationRequested", runId: "trace-run", turnId: "trace-turn-1" }, 906_000),
      mk(traces, 8, {
        type: "contextEntriesApplied",
        baseRevision: 1,
        revision: 2,
        entries: [
          contextReasoning(
            "trace-reasoning-1",
            "**Inspecting the transcript pipeline**\n\nI need to compare the event data with what the web reducer retains. I’ll search for the transcript components, read the reducer, and check the worktree in parallel so I can distinguish missing API data from missing presentation.",
          ),
          ...inspectCalls.map((call, index) =>
            contextToolCallBare(`trace-tool-${index + 1}`, call.callId, call.toolName)),
        ],
      }, 907_000),
      mk(traces, 9, { type: "turnGenerationCompleted", runId: "trace-run", turnId: "trace-turn-1", status: "succeeded" }, 908_000),
      mk(traces, 10, { type: "turnCompleted", turnId: "trace-turn-1" }, 909_000),
      mk(traces, 11, { type: "toolBatchStarted", runId: "trace-run", turnId: "trace-turn-1", batchId: "trace-batch-1", calls: inspectCalls }, 910_000),
      ...inspectCalls.map((call, index) => mk(traces, 12 + index, {
        type: "toolCallStarted",
        runId: "trace-run",
        turnId: "trace-turn-1",
        batchId: "trace-batch-1",
        callId: call.callId,
      }, 911_000 + index)),
      ...inspectCalls.map((call, index) => mk(traces, 15 + index, {
        type: "toolCallCompleted",
        runId: "trace-run",
        turnId: "trace-turn-1",
        batchId: "trace-batch-1",
        callId: call.callId,
        status: "succeeded",
      }, 914_000 + index)),
      mk(traces, 18, {
        type: "contextEntriesApplied",
        baseRevision: 2,
        revision: 3,
        entries: [
          contextToolResult("trace-result-search", "trace-call-search", "platform/web/src/pages/SessionsPage.tsx:987: entries.map(...)\nplatform/web/src/lib/sessions/transcript.ts:72: applyEvents(...)"),
          contextToolResult("trace-result-read", "trace-call-read", "The reducer currently keeps messages and minimal tool rows, but drops reasoningState entries and tool batch display metadata."),
          contextToolResult("trace-result-status", "trace-call-status", " M platform/web/src/lib/sessions/transcript.ts\n?? platform/web/src/components/session/tool-trace.tsx"),
        ],
      }, 917_000),
      mk(traces, 19, { type: "toolBatchCompleted", runId: "trace-run", turnId: "trace-turn-1", batchId: "trace-batch-1" }, 918_000),
      mk(traces, 20, { type: "turnStarted", runId: "trace-run", turnId: "trace-turn-2" }, 919_000),
      mk(traces, 21, { type: "turnGenerationRequested", runId: "trace-run", turnId: "trace-turn-2" }, 920_000),
      mk(traces, 22, {
        type: "contextEntriesApplied",
        baseRevision: 3,
        revision: 4,
        entries: [
          contextReasoning(
            "trace-reasoning-2",
            "**Checking an external dependency**\n\nThe local data explains most of the issue. I’ll still check the deployment health endpoint; if it fails, the failure itself should remain prominent and expandable instead of disappearing into a generic marker.",
          ),
          contextToolCallBare("trace-tool-4", failedCall.callId, failedCall.toolName),
        ],
      }, 921_000),
      mk(traces, 23, { type: "turnGenerationCompleted", runId: "trace-run", turnId: "trace-turn-2", status: "succeeded" }, 922_000),
      mk(traces, 24, { type: "turnCompleted", turnId: "trace-turn-2" }, 923_000),
      mk(traces, 25, { type: "toolBatchStarted", runId: "trace-run", turnId: "trace-turn-2", batchId: "trace-batch-2", calls: [failedCall] }, 924_000),
      mk(traces, 26, { type: "toolCallStarted", runId: "trace-run", turnId: "trace-turn-2", batchId: "trace-batch-2", callId: failedCall.callId }, 925_000),
      mk(traces, 27, { type: "toolCallCompleted", runId: "trace-run", turnId: "trace-turn-2", batchId: "trace-batch-2", callId: failedCall.callId, status: "failed" }, 926_000),
      mk(traces, 28, {
        type: "contextEntriesApplied",
        baseRevision: 4,
        revision: 5,
        entries: [contextToolError("trace-result-health", failedCall.callId, "request failed: DNS name example.invalid could not be resolved")],
      }, 927_000),
      mk(traces, 29, { type: "toolBatchCompleted", runId: "trace-run", turnId: "trace-turn-2", batchId: "trace-batch-2" }, 928_000),
      mk(traces, 30, { type: "turnStarted", runId: "trace-run", turnId: "trace-turn-3" }, 929_000),
      mk(traces, 31, { type: "turnGenerationRequested", runId: "trace-run", turnId: "trace-turn-3" }, 930_000),
      mk(traces, 32, {
        type: "contextEntriesApplied",
        baseRevision: 5,
        revision: 6,
        entries: [
          contextReasoning(
            "trace-reasoning-3",
            "**Forming the conclusion**\n\nThe engine already supplies enough information. The fix belongs in event folding and presentation: retain readable reasoning, merge tool lifecycle events with results, and expose full details on demand.",
          ),
          contextMessage(
            "trace-assistant",
            "The engine data is healthy. I found that the web transcript was discarding reasoning summaries and the semantic metadata attached to tool batches. The richer trace view now keeps both, groups parallel work, and makes arguments and results expandable.",
          ),
        ],
      }, 931_000),
      mk(traces, 33, { type: "turnGenerationCompleted", runId: "trace-run", turnId: "trace-turn-3", status: "succeeded" }, 932_000),
      mk(traces, 34, { type: "turnCompleted", turnId: "trace-turn-3" }, 933_000),
      mk(traces, 35, { type: "runCompleted", runId: "trace-run", outputRef: null }, 934_000),
    ],
  ));
}

function universeView(universeId) {
  const meta = universes.get(universeId) ?? { createdAtMs: 0 };
  return {
    universeId,
    createdAtMs: meta.createdAtMs,
    lastActivityAtMs: null,
    sessions: 0,
    workspaces: prefixed(workspaces, universeId).length,
    profiles: prefixed(profiles, universeId).length,
    blobBytes: 0,
  };
}

server.listen(port, "127.0.0.1", () =>
  console.log(`stub gateway listening on 127.0.0.1:${port}`),
);
