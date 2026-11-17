import { and, eq } from "drizzle-orm";
import { Hono } from "hono";
import { HTTPException } from "hono/http-exception";
import { schema } from "@lightspeed-ai/platform-db";
import type { OperationId } from "@lightspeed-ai/sdk";
import type { AppContext, ApiVariables } from "../context.js";
import { getAuth, requireCredentialManagement } from "../auth-context.js";
import { requireOrganizationAction } from "../organization-access.js";
import { createServiceUserCredential, revokeServiceUserCredential } from "../credential-routes.js";
import { auditAsSubject } from "../identity-audit.js";
import { withApiErrors } from "../http.js";
import { universeClient } from "../runtime-client.js";
import { universeForRequest } from "./universes.js";

/** Configuration only: no session execution, credential management, organization
 * administration or universe lifecycle. New operations require explicit review. */
export const CONFIGURATOR_OPERATIONS = [
  "initialize", "profiles/create", "profiles/read", "profiles/list", "profiles/put", "profiles/delete",
  "mcp/servers/put", "mcp/servers/read", "mcp/servers/list", "mcp/servers/delete", "mcp/servers/tools/discover",
  "models/defaults/read", "models/defaults/put", "models/list",
  "environments/read", "environments/list", "environments/templates/list", "environments/templates/read",
  "environments/provider-bindings/list", "environments/provider-bindings/read",
  "bots/create", "bots/read", "bots/list", "bots/put", "bots/delete",
] satisfies OperationId[];
const SETUP_ID = "configurator";
const VERSION = 6;
const definition = { id: SETUP_ID, name: "Configurator", version: VERSION,
  description: "Configure profiles, MCP servers, models and bots using a service user restricted to this universe." };

export function setupRoutes(ctx: AppContext) {
  const app = new Hono<{ Variables: ApiVariables }>();
  app.get("/:id/setups", async c => {
    const access = await universeForRequest(ctx, c, c.req.param("id"));
    if (!access || !["owner", "operator"].includes(access.role)) return c.json({ error: "not found" }, 404);
    const [installation] = await ctx.db.select().from(schema.universeSetupInstallations)
      .where(and(eq(schema.universeSetupInstallations.universeId, access.universe.id), eq(schema.universeSetupInstallations.setupId, SETUP_ID)));
    const available = Boolean(ctx.env.configuratorMcpUrl);
    let status = installation?.status ?? "available";
    let error = installation?.error;
    if (installation?.status === "ready") {
      const [credential] = installation.state.credentialId ? await ctx.db.select({ enabled: schema.apikey.enabled, expiresAt: schema.apikey.expiresAt }).from(schema.apikey).where(eq(schema.apikey.id, installation.state.credentialId)) : [];
      const [serviceUser] = installation.state.serviceUserId ? await ctx.db.select().from(schema.serviceUsers).where(eq(schema.serviceUsers.id, installation.state.serviceUserId)) : [];
      if (!credential?.enabled || !credential.expiresAt || credential.expiresAt.getTime() <= Date.now() || !serviceUser?.enabled) {
        status = "failed";
        error = "The managed credential is expired or disabled; reinstall to rotate it";
      }
    }
    return c.json([{ ...definition, available, status: available ? status : "unavailable",
      ...(installation ? { installedVersion: installation.installedVersion, error,
        resources: { grantId: installation.state.grantId, serverId: installation.state.serverId } } : {}) }]);
  });
  app.post("/:id/setups/configurator/install", async c => {
    const access = await universeForRequest(ctx, c, c.req.param("id"));
    if (!access) return c.json({ error: "not found" }, 404);
    if (!access.actions.has("administer")) return c.json({ error: "universe permission required: administer" }, 403);
    const auth = getAuth(c);
    requireCredentialManagement(auth);
    // Installation creates a service user and its key in the organization.
    await requireOrganizationAction(ctx, auth, access.organization.id, "manageServiceUsers");
    if (!ctx.env.configuratorMcpUrl) return c.json({ error: "Configurator endpoint is not configured" }, 503);
    const installation = await ctx.db.transaction(async tx => {
      // Shared organization lock serializes installation reservation with service
      // user mutations. Secrets never enter installation state.
      const [organization] = await tx.select().from(schema.organization).where(eq(schema.organization.id, access.organization.id)).for("update");
      if (!organization) throw new HTTPException(404, { message: "Organization not found" });
      const [existing] = await tx.select().from(schema.universeSetupInstallations).where(and(
        eq(schema.universeSetupInstallations.universeId, access.universe.id), eq(schema.universeSetupInstallations.setupId, SETUP_ID)));
      if (existing?.status === "installing" && Date.now() - existing.updatedAt.getTime() < 300_000) throw new HTTPException(409, { message: "Installation is already running" });
      // Keep the working service user valid until the replacement MCP record
      // is ready. A stale attempt carries the original working installation.
      const previous = existing?.status === "ready" ? existing.state : existing?.state.previous;
      if (existing?.status === "installing" && existing.state.serviceUserId) await tx.update(schema.serviceUsers)
        .set({ enabled: false, updatedAt: new Date() }).where(and(eq(schema.serviceUsers.id, existing.state.serviceUserId), eq(schema.serviceUsers.organizationId, organization.id)));
      const [serviceUser] = await tx.insert(schema.serviceUsers).values({ organizationId: organization.id,
        name: `Configurator · ${access.universe.name}`, role: "member" }).returning();
      await tx.insert(schema.universeGrants).values({ universeId: access.universe.id, serviceUserId: serviceUser!.id, role: "operator" });
      const state = { ...(previous ? { previous } : {}), serviceUserId: serviceUser!.id, grantId: `configurator-${serviceUser!.id}`, serverId: "configurator" };
      const [reserved] = await tx.insert(schema.universeSetupInstallations).values({ universeId: access.universe.id,
        setupId: SETUP_ID, status: "installing", state, installedByUserId: auth.principal.id })
        .onConflictDoUpdate({ target: [schema.universeSetupInstallations.universeId, schema.universeSetupInstallations.setupId],
          set: { status: "installing", state, error: null, updatedAt: new Date(), installedByUserId: auth.principal.id } }).returning();
      await auditAsSubject(tx, auth, { action: "setup.install", targetId: serviceUser!.id, universeId: access.universe.id,
        details: { setupId: SETUP_ID, organizationId: organization.id } });
      return reserved!;
    });
    return withApiErrors(c, async () => {
      let keyId: string | undefined;
      try {
        const issued = await createServiceUserCredential(ctx, auth, access.organization.id, installation.state.serviceUserId!, {
          name: "Configurator", restrictions: { organizationIds: [access.organization.id], universeIds: [access.universe.lightspeedUniverseId],
            operations: [...CONFIGURATOR_OPERATIONS], allowUniverseCreation: false },
        });
        keyId = issued.credential.id;
        const client = universeClient(ctx.env, access.universe, access.caller);
        await client.call("credentials/grants/import", { grantId: installation.state.grantId!, displayName: "Configurator Platform credential",
          token: issued.secret, exposure: "brokered", ...(issued.credential.expiresAt ? { expiresAtMs: new Date(issued.credential.expiresAt).getTime() } : {}) }, { signal: AbortSignal.timeout(20_000) });
        await ctx.db.transaction(async tx => {
          const [organization] = await tx.select().from(schema.organization).where(eq(schema.organization.id, access.organization.id)).for("update");
          const [current] = await tx.select().from(schema.universeSetupInstallations).where(eq(schema.universeSetupInstallations.id, installation.id)).for("update");
          if (!organization || current?.state.serviceUserId !== installation.state.serviceUserId) throw new HTTPException(409, { message: "Installation was superseded" });
          // Serialize the final external write with retries and service user mutations.
          // The timeout bounds how long the organization row can remain locked.
          await client.call("mcp/servers/put", { server: { serverId: "configurator", defaultServerLabel: "configurator",
            displayName: "Configurator", serverUrl: ctx.env.configuratorMcpUrl!,
            allowedTools: CONFIGURATOR_OPERATIONS.filter(operation => operation !== "initialize").map(operation => `lightspeed_${operation.replaceAll("/", "_")}`), authPolicy: { type: "requiredBearer" },
            credential: { type: "authGrant", grantId: installation.state.grantId! }, execution: "runtime", approval: "always",
            allowPrivateNetwork: ctx.env.configuratorMcpAllowPrivateNetwork } }, { signal: AbortSignal.timeout(20_000) });
          if (installation.state.previous?.serviceUserId) await tx.update(schema.serviceUsers).set({ enabled: false, updatedAt: new Date() })
            .where(and(eq(schema.serviceUsers.id, installation.state.previous.serviceUserId), eq(schema.serviceUsers.organizationId, access.organization.id)));
          const { previous: _previous, ...readyState } = installation.state;
          await tx.update(schema.universeSetupInstallations).set({ status: "ready", installedVersion: VERSION,
            state: { ...readyState, credentialId: keyId }, updatedAt: new Date() }).where(eq(schema.universeSetupInstallations.id, installation.id));
        });
        return c.json({ ...definition, available: true, status: "ready", installedVersion: VERSION,
          resources: { serverId: "configurator", grantId: installation.state.grantId } });
      } catch (error) {
        // Invalidate admission even if runtime installation or key revocation
        // fails. Keep only a safe diagnostic; upstream errors can include data.
        await ctx.db.transaction(async tx => {
          await tx.select().from(schema.organization).where(eq(schema.organization.id, access.organization.id)).for("update");
          await tx.update(schema.serviceUsers).set({ enabled: false, updatedAt: new Date() }).where(eq(schema.serviceUsers.id, installation.state.serviceUserId!));
          const [current] = await tx.select().from(schema.universeSetupInstallations).where(eq(schema.universeSetupInstallations.id, installation.id)).for("update");
          if (current?.state.serviceUserId === installation.state.serviceUserId) {
            const previous = installation.state.previous;
            // A runtime write can succeed before the database commit fails.
            // Restore the previous MCP binding while the retry lock is held.
            let restored = !previous;
            if (previous?.grantId) {
              try {
                await universeClient(ctx.env, access.universe, access.caller).call("mcp/servers/put", { server: {
                  serverId: "configurator", defaultServerLabel: "configurator", displayName: "Configurator",
                  serverUrl: ctx.env.configuratorMcpUrl!, authPolicy: { type: "requiredBearer" },
                  credential: { type: "authGrant", grantId: previous.grantId }, execution: "runtime", approval: "always",
                  allowedTools: CONFIGURATOR_OPERATIONS.filter(operation => operation !== "initialize").map(operation => `lightspeed_${operation.replaceAll("/", "_")}`),
                  allowPrivateNetwork: ctx.env.configuratorMcpAllowPrivateNetwork,
                } }, { signal: AbortSignal.timeout(20_000) });
                restored = true;
              } catch { /* Keep the previous service user valid for a later repair. */ }
            }
            await tx.update(schema.universeSetupInstallations).set({ status: previous && restored ? "ready" : "failed",
              state: previous && restored ? previous : installation.state,
              error: previous && restored ? "Replacement failed; the previous installation remains active" : "Installation failed; retry to rotate the credential", updatedAt: new Date() })
              .where(eq(schema.universeSetupInstallations.id, installation.id));
          }
        });
        if (keyId) await revokeServiceUserCredential(ctx, auth, access.organization.id, installation.state.serviceUserId!, keyId).catch(() => {});
        throw error;
      }
    });
  });
  return app;
}
