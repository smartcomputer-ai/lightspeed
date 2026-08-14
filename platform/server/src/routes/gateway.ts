import { Hono } from "hono";
import { z } from "zod";
import {
  LightspeedClient,
  LightspeedRpcError,
  type AgentProfileInput,
  type AuthGrantImportParams,
  type AuthGrantListParams,
  type AuthProviderCreateParams,
  type EnvironmentCreateParams,
  type EnvironmentListParams,
  type McpServerInput,
  type ModelListParams,
  type ProfileSource,
  type SessionConfig,
} from "@lightspeed/agent-client";
import { schema } from "@lightspeed/platform-db";
import { slugify, workspaceCreateSchema } from "@lightspeed/platform-shared";
import {
  FOUNDRY_PACK_WORKFLOW,
  foundryDirectTerminalToken,
} from "@lightspeed/foundry/contracts";
import type { AppContext, ApiVariables } from "../context.js";
import { parseBody } from "../http.js";
import { universeForSession } from "./universes.js";
import {
  asManifest,
  removeFile,
  setFile,
  VfsPathError,
  type VfsManifest,
} from "../vfs.js";

/// Loose validation only (id present and path-consistent): the gateway is
/// the validator of record for profile documents.
const profileDocumentSchema = z
  .object({ profileId: z.string().min(1) })
  .catchall(z.unknown());

/// One of text or base64 content; `expectedRevision` is the workspace
/// revision the editor loaded — a stale save gets a 409, not a clobber.
/// Fresh workspaces sit at revision 0 (engine truth), so 0 is valid.
const filePutSchema = z
  .object({
    contentText: z.string().optional(),
    contentBase64: z.string().optional(),
    mediaType: z.string().min(1).optional(),
    expectedRevision: z.number().int().min(0),
  })
  .refine((v) => (v.contentText === undefined) !== (v.contentBase64 === undefined), {
    message: "exactly one of contentText or contentBase64 is required",
  });

const looseDocumentSchema = z.object({}).catchall(z.unknown());

const profileSourceSchema = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("named"), profileId: z.string().min(1) }),
  z.object({
    kind: z.literal("inline"),
    profile: looseDocumentSchema,
  }),
]);

/// Session-create body for the web chat's "New session". The web models
/// every setup as a profile source: named while untouched, inline once the
/// user authors or customizes it.
const sessionCreateSchema = z.object({
  displayName: z.string().trim().min(1).max(200).optional(),
  profile: profileSourceSchema,
});

const sessionConfigPutSchema = z.object({
  config: looseDocumentSchema,
  expectedConfigRevision: z.number().int().min(0),
});

const sessionCloseSchema = z.object({
  force: z.boolean().optional(),
});

const sessionInstructionsPutSchema = z.object({
  text: z.string().max(1_000_000).nullable(),
});

const environmentCreateSchema = z.object({
  requestId: z.string().trim().min(1).max(200),
  bindingId: z.string().trim().min(1).max(200),
  templateId: z.string().trim().min(1).max(200),
  displayName: z.string().trim().min(1).max(200).optional(),
  metadata: z.record(z.string(), z.string()).optional(),
});

const environmentIngressPutSchema = z.object({
  enabled: z.boolean(),
});

const environmentCredentialBindSchema = z.object({
  envName: z
    .string()
    .trim()
    .regex(/^[A-Za-z_][A-Za-z0-9_]{0,127}$/, "invalid environment variable name"),
  source: z.discriminatedUnion("type", [
    z.object({ type: z.literal("authGrant"), grantId: z.string().trim().min(1) }),
    z.object({
      type: z.literal("authProviderCredential"),
      providerId: z.string().trim().min(1),
    }),
  ]),
});

/// Secret values are accepted only on these write-only creation paths. The
/// engine encrypts them before persistence and all read routes return metadata
/// and stable handles only.
const providerCredentialCreateSchema = z.object({
  providerId: z.enum(["openai", "anthropic"]),
  displayName: z.string().trim().min(1).max(200).optional(),
  credential: z.string().min(1),
});

const bearerGrantCreateSchema = z.object({
  grantId: z.string().trim().min(1).optional(),
  displayName: z.string().trim().min(1).max(200).optional(),
  subjectHint: z.string().trim().min(1).max(200).optional(),
  token: z.string().min(1),
});

const environmentSecretCreateSchema = z.object({
  grantId: z.string().trim().min(1).optional(),
  displayName: z.string().trim().min(1).max(200).optional(),
  value: z
    .string()
    .min(1)
    .max(1_000_000)
    .refine((value) => !value.includes("\0"), "environment secrets cannot contain NUL bytes"),
});

export const ENVIRONMENT_SECRET_PROVIDER_ID = "environment-secret";

export function environmentSecretGrantParams(
  input: z.infer<typeof environmentSecretCreateSchema>,
): AuthGrantImportParams {
  return {
    grantId: input.grantId,
    providerId: ENVIRONMENT_SECRET_PROVIDER_ID,
    displayName: input.displayName,
    token: input.value,
  };
}

/// One text message = one run. `submissionId` is client-minted so a
/// retried POST (network flake) returns the original run instead of
/// starting a duplicate.
const sessionMessageSchema = z.object({
  text: z.string().min(1).max(100_000),
  submissionId: z.string().min(1).max(200),
});

/// Loose validation (required identifiers only): the engine is the
/// validator of record for MCP server records, like profile documents.
const mcpServerDocumentSchema = z
  .object({
    serverId: z.string().min(1),
    serverUrl: z.string().min(1),
    defaultServerLabel: z.string().min(1),
    revision: z.number().int().min(0).optional(),
    createdAtMs: z.number().optional(),
    updatedAtMs: z.number().optional(),
  })
  .catchall(z.unknown());

type UniverseRow = typeof schema.universes.$inferSelect;

/// Client for universe-scoped calls: stamps `x-lightspeed-universe`
/// (trusted-header mode).
export function engineClientFor(
  ctx: AppContext,
  universe: UniverseRow,
  principal?: string,
): LightspeedClient {
  const endpoint = universe.gatewayUrl ?? ctx.env.lightspeedApiUrl;
  if (!endpoint) {
    throw new GatewayUnconfigured();
  }
  return new LightspeedClient({
    endpoint,
    headers: {
      "x-lightspeed-universe": universe.lightspeedUniverseId,
      ...(principal ? { "x-lightspeed-principal": principal } : {}),
    },
  });
}

/// Client for operator-scoped calls (`operator/*`): no universe header —
/// these address the deployment, and the gateway rejects a universe
/// header on them.
export function operatorClientFor(ctx: AppContext, endpoint?: string | null): LightspeedClient {
  const resolved = endpoint ?? ctx.env.lightspeedApiUrl;
  if (!resolved) {
    throw new GatewayUnconfigured();
  }
  return new LightspeedClient({ endpoint: resolved });
}

/// Universe-scoped passthrough to the Lightspeed gateway: the platform
/// checks membership, the engine owns the documents. Config surfaces are
/// owner/admin-only — unlike bindings, profile documents can embed
/// sensitive engine configuration.
export function gatewayRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();

  app.get("/:id/profiles", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("profiles/list", {});
      return c.json(response.result.profiles ?? []);
    });
  });

  app.get("/:id/profiles/:profileId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("profiles/read", {
        profileId: c.req.param("profileId"),
      });
      return c.json(response.result.profile);
    });
  });

  /// Create-or-replace. A `revision` field in the document (as loaded from
  /// GET) rides along as `expectedRevision` — a stale editor gets a 409
  /// instead of silently clobbering a concurrent edit.
  app.put("/:id/profiles/:profileId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, profileDocumentSchema);
    if (!body.ok) {
      return body.response;
    }
    if (body.data.profileId !== c.req.param("profileId")) {
      return c.json({ error: "profileId in document does not match URL" }, 400);
    }
    const { revision, createdAtMs, updatedAtMs, ...document } = body.data as {
      revision?: number;
      createdAtMs?: number;
      updatedAtMs?: number;
    } & Record<string, unknown>;
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("profiles/put", {
        profile: document as unknown as AgentProfileInput,
        expectedRevision: revision,
      });
      return c.json({ profile: response.result.profile });
    });
  });

  app.delete("/:id/profiles/:profileId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      await client.call("profiles/delete", { profileId: c.req.param("profileId") });
      return c.json({ ok: true });
    });
  });

  /// Paged session summaries, newest activity first (engine keyset
  /// paging). Roots-only/tree filtering arrives with engine D1
  /// (parentSessionId) — today channel-managed and web-created sessions are roots.
  app.get("/:id/sessions", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const cursor = c.req.query("cursor") ?? null;
    const limitRaw = Number(c.req.query("limit") ?? 50);
    const limit = Number.isFinite(limitRaw) ? Math.min(Math.max(1, limitRaw), 200) : 50;
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("session/list", { cursor, limit });
      return c.json({
        sessions: response.result.sessions ?? [],
        nextCursor: response.result.nextCursor ?? null,
      });
    });
  });

  app.get("/:id/sessions/:sessionId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("session/read", {
        sessionId: c.req.param("sessionId"),
      });
      return c.json(response.result.session);
    });
  });

  /// Closing is a lifecycle transition that retains session history.
  /// `force=true` also cancels active/queued work.
  app.post("/:id/sessions/:sessionId/close", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, sessionCloseSchema);
    if (!body.ok) {
      return body.response;
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("session/close", {
        sessionId: c.req.param("sessionId"),
        force: body.data.force ?? false,
      });
      return c.json(response.result.session);
    });
  });

  /// Deletion removes retained history and is accepted by Lightspeed only
  /// after the session is closed (and has no inheriting forks).
  app.delete("/:id/sessions/:sessionId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("session/delete", {
        sessionId: c.req.param("sessionId"),
      });
      return c.json(response.result.session);
    });
  });

  /// Event-log page for the transcript. `after` is the numeric cursor seq
  /// from the previous page; `waitMs` long-polls at the engine (clamped
  /// to 30s) so the web tail follows live sessions without spinning.
  app.get("/:id/sessions/:sessionId/events", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const afterRaw = c.req.query("after");
    const after = afterRaw !== undefined ? Number(afterRaw) : null;
    const limitRaw = Number(c.req.query("limit") ?? 200);
    const limit = Number.isFinite(limitRaw) ? Math.min(Math.max(1, limitRaw), 500) : 200;
    const waitRaw = c.req.query("waitMs");
    const waitMs = waitRaw !== undefined ? Number(waitRaw) : null;
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("session/events/read", {
        sessionId: c.req.param("sessionId"),
        after: after !== null && Number.isFinite(after) ? { seq: after } : null,
        limit,
        waitMs:
          waitMs !== null && Number.isFinite(waitMs)
            ? Math.min(Math.max(0, waitMs), 30_000)
            : null,
      });
      return c.json(response.result);
    });
  });

  /// New session from the web (no chat binding involved). The engine
  /// mints the session id; the response is the full session view.
  app.post("/:id/sessions", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, sessionCreateSchema);
    if (!body.ok) {
      return body.response;
    }
    const input = body.data;
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("session/start", {
        ...(input.displayName ? { displayName: input.displayName } : {}),
        profile: input.profile as ProfileSource,
      });
      return c.json(response.result.session);
    });
  });

  /// Session config is a sparse whole document with optimistic concurrency.
  app.put("/:id/sessions/:sessionId/config", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, sessionConfigPutSchema);
    if (!body.ok) {
      return body.response;
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("session/config/put", {
        sessionId: c.req.param("sessionId"),
        config: body.data.config as SessionConfig,
        expectedConfigRevision: body.data.expectedConfigRevision,
      });
      return c.json(response.result.session);
    });
  });

  /// Session-local custom instructions use the engine's managed profile
  /// instruction source. This deliberately delegates reconciliation to an
  /// instruction-only inline profile apply; raw context edits would not
  /// preserve the default/profile/VFS fallback invariants.
  app.get("/:id/sessions/:sessionId/instructions", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("session/read", {
        sessionId: c.req.param("sessionId"),
      });
      const active = (response.result.session.activeContext.entries ?? [])
        .filter((entry) => entry.kind.type === "instructions")
        .map((entry) => ({
          key: entry.key ?? null,
          contentRef: entry.contentRef,
          preview: entry.preview ?? null,
        }));
      const custom = active.find((entry) => entry.key === "instructions.050.profile");
      let text: string | null = null;
      if (custom) {
        const blob = await client.call("blobs/read", { blobRef: custom.contentRef });
        text = Buffer.from(blob.result.bytesBase64, "base64").toString("utf8");
      }
      return c.json({
        text,
        contextRevision: response.result.session.activeContext.revision,
        active,
      });
    });
  });

  app.put("/:id/sessions/:sessionId/instructions", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, sessionInstructionsPutSchema);
    if (!body.ok) {
      return body.response;
    }
    const text = body.data.text?.trim() ? body.data.text : null;
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("session/profiles/apply", {
        sessionId: c.req.param("sessionId"),
        profile: {
          kind: "inline",
          profile: text
            ? { instructions: { type: "text", text } }
            : {},
        },
      });
      return c.json(response.result.session);
    });
  });

  app.post("/:id/sessions/:sessionId/environments/:environmentId/activate", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("session/environments/activate", {
        sessionId: c.req.param("sessionId"),
        environmentId: c.req.param("environmentId"),
      });
      return c.json(response.result.session);
    });
  });

  app.post("/:id/sessions/:sessionId/environments/deactivate", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("session/environments/deactivate", {
        sessionId: c.req.param("sessionId"),
      });
      return c.json(response.result.session);
    });
  });

  /// One user message → one run from input items. Returns the accepted
  /// run immediately — replies land in the event log, which the web
  /// follows via the long-poll tail. No server-side await: runs can take
  /// minutes and an HTTP request must not.
  app.post("/:id/sessions/:sessionId/messages", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, sessionMessageSchema);
    if (!body.ok) {
      return body.response;
    }
    const input = body.data;
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const session = (await client.call("session/read", {
        sessionId: c.req.param("sessionId"),
      })).result.session;
      const foundryManaged =
        session.management?.lifecycleController?.workflowKind === FOUNDRY_PACK_WORKFLOW;
      const response = await client.call("session/runs/start", {
        sessionId: c.req.param("sessionId"),
        source: { type: "input" as const, items: [{ type: "text" as const, text: input.text }] },
        submissionId: input.submissionId,
        ...(foundryManaged
          ? { notifyOnTerminal: { token: foundryDirectTerminalToken(input.submissionId) } }
          : {}),
      });
      const run = response.result.run;
      return c.json({ run: { id: run.id, status: run.status } });
    });
  });

  app.post("/:id/sessions/:sessionId/runs/:runId/cancel", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      await client.call("session/runs/cancel", {
        sessionId: c.req.param("sessionId"),
        runId: c.req.param("runId"),
      });
      return c.json({ ok: true });
    });
  });

  app.get("/:id/workspaces", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("vfs/workspaces/list", {});
      return c.json(response.result.workspaces ?? []);
    });
  });

  /// MCP server catalog (the U5a page + the profile editor's picker).
  app.get("/:id/mcp-servers", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("mcp/servers/list", {});
      return c.json(response.result.servers ?? []);
    });
  });

  /// Provider-discovered model routes for the session-config model picker.
  /// Lightspeed owns credential injection and sanitizes per-provider errors.
  app.get("/:id/models", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const params: ModelListParams = { selectableOnly: true };
      const response = await client.call("models/list", params);
      return c.json(response.result);
    });
  });

  /// Non-secret active grant metadata for universe MCP-server credential selection.
  app.get("/:id/auth-grants", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const params: AuthGrantListParams = { status: "active" };
      const response = await client.call("auth/grants/list", params);
      return c.json(response.result.grants ?? []);
    });
  });

  /// Universe secret inventory. Values are intentionally absent: providers
  /// expose only `hasCredential`, while grants expose token-presence flags.
  app.get("/:id/secrets", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const [providers, grants] = await Promise.all([
        client.call("auth/providers/list", {}),
        client.call("auth/grants/list", {}),
      ]);
      return c.json({
        providers: (providers.result.providers ?? [])
          .filter((provider) =>
            provider.config.type === "modelApiKey" || provider.config.type === "modelOAuth"
          )
          .map(modelProviderCredentialView),
        grants: grants.result.grants ?? [],
      });
    });
  });

  app.post("/:id/secrets/providers", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, providerCredentialCreateSchema);
    if (!body.ok) {
      return body.response;
    }
    return withGateway(c, async () => {
      const params: AuthProviderCreateParams = {
        providerId: modelProviderCredentialId(body.data.providerId),
        displayName: body.data.displayName,
        config: { type: "modelApiKey" },
        credential: body.data.credential,
      };
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("auth/providers/create", params);
      return c.json(modelProviderCredentialView(response.result.provider), 201);
    });
  });

  app.delete("/:id/secrets/providers/:providerId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("auth/providers/delete", {
        providerId: c.req.param("providerId"),
      });
      return c.json(modelProviderCredentialView(response.result.provider));
    });
  });

  app.post("/:id/secrets/grants", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, bearerGrantCreateSchema);
    if (!body.ok) {
      return body.response;
    }
    return withGateway(c, async () => {
      const params: AuthGrantImportParams = body.data;
      const client = engineClientFor(ctx, access.universe);
      if (body.data.grantId) {
        try {
          const existing = await client.call("auth/grants/read", {
            grantId: body.data.grantId,
          });
          return c.json(
            { error: credentialIdConflictMessage(body.data.grantId, existing.result.grant.status) },
            409,
          );
        } catch (error) {
          if (!(error instanceof LightspeedRpcError && error.kind === "not_found")) {
            throw error;
          }
        }
      }
      try {
        const response = await client.call("auth/grants/import", params);
        return c.json(response.result.grant, 201);
      } catch (error) {
        if (body.data.grantId && error instanceof LightspeedRpcError && error.kind === "conflict") {
          return c.json(
            { error: credentialIdConflictMessage(body.data.grantId) },
            409,
          );
        }
        throw error;
      }
    });
  });

  /// Opaque, multiline-safe values for environment-variable injection. The
  /// engine's static-grant path is the encrypted write-only primitive; the
  /// dedicated provider id keeps these distinct from actual bearer tokens.
  app.post("/:id/secrets/environment", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, environmentSecretCreateSchema);
    if (!body.ok) {
      return body.response;
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      if (body.data.grantId) {
        try {
          const existing = await client.call("auth/grants/read", {
            grantId: body.data.grantId,
          });
          return c.json(
            { error: credentialIdConflictMessage(body.data.grantId, existing.result.grant.status) },
            409,
          );
        } catch (error) {
          if (!(error instanceof LightspeedRpcError && error.kind === "not_found")) {
            throw error;
          }
        }
      }
      try {
        const response = await client.call(
          "auth/grants/import",
          environmentSecretGrantParams(body.data),
        );
        return c.json(response.result.grant, 201);
      } catch (error) {
        if (body.data.grantId && error instanceof LightspeedRpcError && error.kind === "conflict") {
          return c.json(
            { error: credentialIdConflictMessage(body.data.grantId) },
            409,
          );
        }
        throw error;
      }
    });
  });

  app.delete("/:id/secrets/grants/:grantId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("auth/grants/revoke", {
        grantId: c.req.param("grantId"),
      });
      return c.json(response.result.grant);
    });
  });

  app.post("/:id/mcp-servers", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, mcpServerDocumentSchema);
    if (!body.ok) {
      return body.response;
    }
    const { revision, createdAtMs, updatedAtMs, ...input } = body.data;
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("mcp/servers/put", {
        server: input as unknown as McpServerInput,
      });
      return c.json(response.result.server, 201);
    });
  });

  /// Create-or-replace, mirroring the engine's mcp/servers/put semantics.
  /// A `revision` field from GET is forwarded as the CAS guard.
  app.put("/:id/mcp-servers/:serverId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, mcpServerDocumentSchema);
    if (!body.ok) {
      return body.response;
    }
    if (body.data.serverId !== c.req.param("serverId")) {
      return c.json({ error: "serverId in document does not match URL" }, 400);
    }
    const { revision, createdAtMs, updatedAtMs, ...server } = body.data;
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("mcp/servers/put", {
        server: server as unknown as McpServerInput,
        expectedRevision: revision,
      });
      return c.json(response.result.server);
    });
  });

  app.delete("/:id/mcp-servers/:serverId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      await client.call("mcp/servers/delete", { serverId: c.req.param("serverId") });
      return c.json({ ok: true });
    });
  });

  /// Universe-scoped admission bindings. Physical provider registration is
  /// deployment/operator state and is never exposed through this member API.
  app.get("/:id/environment-provider-bindings", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("environments/provider-bindings/list", {});
      return c.json(response.result.bindings);
    });
  });

  app.get("/:id/environment-templates", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const bindingId = c.req.query("bindingId");
      if (bindingId) {
        const response = await client.call("environments/templates/list", { bindingId });
        return c.json(response.result.templates);
      }
      const bindings = await client.call("environments/provider-bindings/list", {});
      const results = await Promise.allSettled(
        bindings.result.bindings
          .filter((binding) => binding.status === "enabled")
          .map((binding) => client.call("environments/templates/list", {
            bindingId: binding.bindingId,
          })),
      );
      return c.json(results.flatMap((result) =>
        result.status === "fulfilled" ? result.value.result.templates : []
      ));
    });
  });

  app.get("/:id/environments", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const params: EnvironmentListParams = {};
      const providerId = c.req.query("providerId");
      if (providerId) {
        params.providerId = providerId;
      }
      const bindingId = c.req.query("bindingId");
      if (bindingId) {
        params.bindingId = bindingId;
      }
      const status = c.req.query("status");
      if (status) {
        params.status = status as EnvironmentListParams["status"];
      }
      const response = await client.call("environments/list", params);
      return c.json(response.result.environments ?? []);
    });
  });

  app.post("/:id/environments", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, environmentCreateSchema);
    if (!body.ok) {
      return body.response;
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call(
        "environments/create",
        body.data as EnvironmentCreateParams,
      );
      return c.json(response.result.environment, 201);
    });
  });

  app.put("/:id/environments/:environmentId/ingress", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, environmentIngressPutSchema);
    if (!body.ok) {
      return body.response;
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("environments/ingress/put", {
        environmentId: c.req.param("environmentId"),
        enabled: body.data.enabled,
      });
      return c.json(response.result.environment);
    });
  });

  app.get("/:id/environments/:environmentId/credentials", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("environments/credentials/list", {
        environmentId: c.req.param("environmentId"),
      });
      return c.json(response.result.credentials ?? []);
    });
  });

  app.post("/:id/environments/:environmentId/credentials", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, environmentCredentialBindSchema);
    if (!body.ok) {
      return body.response;
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("environments/credentials/bind", {
        environmentId: c.req.param("environmentId"),
        envName: body.data.envName,
        source: body.data.source,
      });
      return c.json(response.result.credential, 201);
    });
  });

  app.delete("/:id/environments/:environmentId/credentials/:envName", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("environments/credentials/unbind", {
        environmentId: c.req.param("environmentId"),
        envName: c.req.param("envName"),
      });
      return c.json(response.result.credential);
    });
  });

  app.get("/:id/environments/:environmentId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("environments/read", {
        environmentId: c.req.param("environmentId"),
      });
      return c.json(response.result.environment);
    });
  });

  app.delete("/:id/environments/:environmentId", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("environments/close", {
        environmentId: c.req.param("environmentId"),
      });
      return c.json(response.result.environment);
    });
  });

  /// Workspace head + full manifest in one roundtrip (the explorer tree).
  app.get("/:id/workspaces/:workspaceId/tree", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const workspace = await client.call("vfs/workspaces/read", {
        workspaceId: c.req.param("workspaceId"),
      });
      const snapshot = await client.call("vfs/snapshots/read", {
        snapshotRef: workspace.result.workspace.headSnapshotRef,
      });
      return c.json({
        workspace: workspace.result.workspace,
        manifest: snapshot.result.manifest,
      });
    });
  });

  app.get("/:id/blobs/:blobRef", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const response = await client.call("blobs/read", {
        blobRef: c.req.param("blobRef"),
      });
      return c.json(response.result);
    });
  });

  /// Write a file: upload the blob, graft it into the head manifest,
  /// commit the new snapshot, advance the head at `expectedRevision`.
  /// The engine validates the manifest and enforces the revision (409).
  app.put("/:id/workspaces/:workspaceId/files/:path{.+}", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, filePutSchema);
    if (!body.ok) {
      return body.response;
    }
    const bytesBase64 =
      body.data.contentBase64 ??
      Buffer.from(body.data.contentText ?? "", "utf8").toString("base64");
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const manifest = await headManifestAt(
        client,
        c.req.param("workspaceId"),
        body.data.expectedRevision,
      );
      if (manifest instanceof Response) {
        return manifest;
      }
      const put = await client.call("blobs/put", { blobs: [{ bytesBase64 }] });
      const stored = put.result.blobs?.[0];
      if (!stored) {
        return c.json({ error: "engine error: blob upload returned nothing" }, 502);
      }
      try {
        setFile(manifest, c.req.param("path"), {
          kind: "file",
          blob_ref: stored.blobRef,
          size_bytes: stored.bytes,
          ...(body.data.mediaType ? { media_type: body.data.mediaType } : {}),
          executable: false,
        });
      } catch (error) {
        if (error instanceof VfsPathError) {
          return c.json({ error: error.message }, 400);
        }
        throw error;
      }
      return c.json(
        await commitHead(client, c.req.param("workspaceId"), manifest, body.data.expectedRevision),
      );
    });

    async function headManifestAt(
      client: LightspeedClient,
      workspaceId: string,
      expectedRevision: number,
    ): Promise<VfsManifest | Response> {
      const workspace = await client.call("vfs/workspaces/read", { workspaceId });
      if (workspace.result.workspace.revision !== expectedRevision) {
        return c.json(
          { error: "workspace changed since it was loaded — reload and retry" },
          409,
        );
      }
      const snapshot = await client.call("vfs/snapshots/read", {
        snapshotRef: workspace.result.workspace.headSnapshotRef,
      });
      return asManifest(snapshot.result.manifest);
    }
  });

  app.delete("/:id/workspaces/:workspaceId/files/:path{.+}", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const expectedRevision = Number(c.req.query("expectedRevision"));
    if (!Number.isInteger(expectedRevision) || expectedRevision < 0) {
      return c.json({ error: "expectedRevision query parameter is required" }, 400);
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      const workspaceId = c.req.param("workspaceId");
      const workspace = await client.call("vfs/workspaces/read", { workspaceId });
      if (workspace.result.workspace.revision !== expectedRevision) {
        return c.json(
          { error: "workspace changed since it was loaded — reload and retry" },
          409,
        );
      }
      const snapshot = await client.call("vfs/snapshots/read", {
        snapshotRef: workspace.result.workspace.headSnapshotRef,
      });
      const manifest = asManifest(snapshot.result.manifest);
      if (!removeFile(manifest, c.req.param("path"))) {
        return c.json({ error: "file not found" }, 404);
      }
      return c.json(await commitHead(client, workspaceId, manifest, expectedRevision));
    });
  });

  app.post("/:id/workspaces", async (c) => {
    const access = await universeForSession(ctx, c, c.req.param("id"), true);
    if (!access) {
      return c.json({ error: "not found" }, 404);
    }
    const body = await parseBody(c, workspaceCreateSchema);
    if (!body.ok) {
      return body.response;
    }
    return withGateway(c, async () => {
      const client = engineClientFor(ctx, access.universe);
      // No snapshotRef: the engine starts the workspace from the empty
      // snapshot. Friendly id derived from the display name (profile
      // workspace links reference workspaceId — "notes" beats an opaque mint);
      // nothing given → the engine mints one.
      const workspaceId =
        body.data.workspaceId ??
        (body.data.displayName ? slugify(body.data.displayName) : null);
      const response = await client.call("vfs/workspaces/create", {
        workspaceId,
        displayName: body.data.displayName ?? null,
      });
      return c.json(response.result.workspace, 201);
    });
  });

  return app;
}

export function modelProviderCredentialId(providerId: string): string {
  return providerId.startsWith("model:") ? providerId : `model:${providerId}`;
}

export function credentialIdConflictMessage(grantId: string, status?: string): string {
  if (status === "revoked") {
    return `credential ID "${grantId}" belongs to a revoked access credential and cannot be reused; leave it blank to generate a new ID or choose another`;
  }
  const state = status ? `an access credential with status "${status}"` : "another access credential";
  return `credential ID "${grantId}" already belongs to ${state}; leave it blank to generate a new ID or choose another`;
}

/// The engine deliberately namespaces model credentials as
/// `model:<ModelSelection.providerId>`. Keep that storage detail out of the UI
/// while retaining the full row id for unambiguous removal and legacy cleanup.
export function modelProviderCredentialView<
  T extends { providerId: string; config: { type: string } },
>(provider: T) {
  const credentialId = provider.providerId;
  const providerId = credentialId.startsWith("model:")
    ? credentialId.slice("model:".length)
    : credentialId;
  return {
    ...provider,
    providerId,
    credentialId,
    usableForModels: credentialId === modelProviderCredentialId(providerId),
  };
}

/// Commit the edited manifest and advance the workspace head at the
/// loaded revision. Engine-side revision check is the real guard — the
/// earlier read-time check only gives a friendlier error.
async function commitHead(
  client: LightspeedClient,
  workspaceId: string,
  manifest: VfsManifest,
  expectedRevision: number,
) {
  const commit = await client.call("vfs/snapshots/commit", { manifest });
  const updated = await client.call("vfs/workspaces/update", {
    workspaceId,
    snapshotRef: commit.result.snapshotRef,
    expectedRevision,
  });
  return { workspace: updated.result.workspace };
}

class GatewayUnconfigured extends Error {
  constructor() {
    super("no gateway endpoint configured (LIGHTSPEED_API_URL)");
  }
}

/// Maps gateway failures onto API responses: engine not_found/conflict pass
/// through as 404/409, transport or internal errors surface as 502.
export async function withGateway(
  c: { json: (body: unknown, status?: 400 | 404 | 409 | 501 | 502) => Response },
  fn: () => Promise<Response>,
): Promise<Response> {
  try {
    return await fn();
  } catch (error) {
    if (error instanceof GatewayUnconfigured) {
      return c.json({ error: error.message }, 501);
    }
    if (error instanceof LightspeedRpcError) {
      if (error.kind === "not_found") {
        return c.json({ error: "not found in engine" }, 404);
      }
      if (error.kind === "conflict") {
        return c.json({ error: `engine conflict: ${error.message}` }, 409);
      }
      return c.json({ error: `engine error: ${error.message}` }, 502);
    }
    return c.json(
      { error: error instanceof Error ? error.message : String(error) },
      502,
    );
  }
}
