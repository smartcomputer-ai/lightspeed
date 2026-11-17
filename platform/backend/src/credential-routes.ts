import { and, desc, eq, inArray } from "drizzle-orm";
import { Hono, type Handler } from "hono";
import { bodyLimit } from "hono/body-limit";
import { HTTPException } from "hono/http-exception";
import { schema } from "@lightspeed-ai/platform-db";
import { CREDENTIAL_OPERATIONS, ORGANIZATION_ROLES, credentialCreateSchema, credentialRestrictionsSchema } from "@lightspeed-ai/platform-shared";
import { z } from "zod";
import type { ApiVariables, AppContext } from "./context.js";
import { getAuth, requireCredentialManagement, type AuthContext } from "./auth-context.js";
import { declaredPrincipalRole, requireOrganizationAction } from "./organization-access.js";
import { hasPlatformAdminRole } from "./context.js";
import { PERSONAL_KEY_CONFIG, SERVICE_USER_KEY_CONFIG } from "./api-key-config.js";
import { auditIdentity } from "./identity-audit.js";

type Owner = { userId: string; serviceUserId?: never; organizationId?: never } | { userId?: never; serviceUserId: string; organizationId: string };
type Create = z.infer<typeof credentialCreateSchema>;
const publicKeyColumns = {
  id: schema.apikey.id, name: schema.apikey.name, prefix: schema.apikey.prefix, start: schema.apikey.start,
  createdAt: schema.apikey.createdAt, expiresAt: schema.apikey.expiresAt, enabled: schema.apikey.enabled,
  lastUsedAt: schema.apikey.lastRequest, restrictions: schema.credentialBindings.restrictions,
};

function metadata(row: {
  id: string; name: string | null; prefix: string | null; start: string | null; createdAt: Date;
  expiresAt: Date | null; enabled: boolean; lastUsedAt: Date | null; restrictions: unknown;
}) {
  return { id: row.id, name: row.name ?? "API key", prefix: row.prefix, start: row.start, enabled: row.enabled,
    createdAt: row.createdAt.toISOString(), expiresAt: row.expiresAt?.toISOString() ?? null,
    lastUsedAt: row.lastUsedAt?.toISOString() ?? null, restrictions: credentialRestrictionsSchema.parse(row.restrictions) };
}

function ownerConditions(owner: Owner) {
  return owner.serviceUserId ? and(eq(schema.credentialBindings.serviceUserId, owner.serviceUserId),
    eq(schema.apikey.configId, SERVICE_USER_KEY_CONFIG), eq(schema.apikey.referenceId, owner.organizationId)) :
    and(eq(schema.credentialBindings.userId, owner.userId!), eq(schema.apikey.configId, PERSONAL_KEY_CONFIG), eq(schema.apikey.referenceId, owner.userId!));
}

async function listCredentials(ctx: AppContext, owner: Owner) {
  const rows = await ctx.db.select(publicKeyColumns).from(schema.credentialBindings)
    .innerJoin(schema.apikey, eq(schema.apikey.id, schema.credentialBindings.keyId))
    .where(ownerConditions(owner)).orderBy(desc(schema.apikey.createdAt));
  return { credentials: rows.map(metadata) };
}

async function createCredential(ctx: AppContext, auth: AuthContext, owner: Owner, input: Create) {
  // A key never expires only when asked explicitly (null), and only where the deployment sets no maximum.
  const maximum = ctx.env.apiKeyMaxExpiryDays;
  const expiresInDays = input.expiresInDays === undefined ? Math.min(ctx.env.apiKeyDefaultExpiryDays ?? 365, maximum ?? Infinity) : input.expiresInDays;
  if (expiresInDays === null && maximum !== undefined) throw new HTTPException(400, { message: `This deployment requires keys to expire within ${maximum} days` });
  if (expiresInDays !== null && maximum !== undefined && expiresInDays > maximum) throw new HTTPException(400, { message: "Credential expiry exceeds deployment policy" });
  const configId = owner.serviceUserId ? SERVICE_USER_KEY_CONFIG : PERSONAL_KEY_CONFIG;
  const create = async () => {
    const key = await ctx.auth.api.createApiKey({ body: {
      configId, userId: auth.principal.id, name: input.name, expiresIn: expiresInDays === null ? null : expiresInDays * 86400,
      ...(owner.serviceUserId ? { organizationId: owner.organizationId } : {}),
    } });
    try {
      await ctx.db.transaction(async (tx) => {
        await tx.insert(schema.credentialBindings).values({ keyId: key.id, userId: owner.userId,
          serviceUserId: owner.serviceUserId, restrictions: input.restrictions, createdBy: auth.actorId });
        await auditIdentity(tx, { actorId: auth.actorId, action: "credential.create", targetId: key.id,
          details: { principalKind: owner.serviceUserId ? "serviceUser" : "user", principalId: owner.serviceUserId ?? owner.userId!, credentialId: auth.credential.id } });
      });
    } catch (error) {
      // Unassociated keys are always refused by admission, including after a process crash.
      await ctx.auth.api.updateApiKey({ body: { configId, keyId: key.id, userId: auth.principal.id, enabled: false } }).catch(() => {});
      throw error;
    }
    return { credential: metadata({ ...key, lastUsedAt: key.lastRequest, restrictions: input.restrictions }), secret: key.key };
  };
  return owner.serviceUserId ? ctx.auth.withOrganizationKeyAuthority({ userId: auth.principal.id, organizationId: owner.organizationId }, create) : create();
}

async function revokeCredential(ctx: AppContext, auth: AuthContext, owner: Owner, keyId: string) {
  const [key] = await ctx.db.select({ id: schema.apikey.id, configId: schema.apikey.configId }).from(schema.credentialBindings)
    .innerJoin(schema.apikey, eq(schema.apikey.id, schema.credentialBindings.keyId))
    .where(and(ownerConditions(owner), eq(schema.apikey.id, keyId)));
  if (!key) throw new HTTPException(404, { message: "Credential not found" });
  const revoke = () => ctx.auth.api.updateApiKey({ body: { configId: key.configId, keyId, userId: auth.principal.id, enabled: false } });
  if (owner.serviceUserId) await ctx.auth.withOrganizationKeyAuthority({ userId: auth.principal.id, organizationId: owner.organizationId }, revoke);
  else await revoke();
  await ctx.db.transaction(async (tx) => {
    await tx.delete(schema.credentialBindings).where(eq(schema.credentialBindings.keyId, keyId));
    await auditIdentity(tx, { actorId: auth.actorId, action: "credential.revoke", targetId: keyId,
      details: { principalKind: owner.serviceUserId ? "serviceUser" : "user", principalId: owner.serviceUserId ?? owner.userId!, credentialId: auth.credential.id } });
  });
  return { revoked: true as const };
}

/** Internal callers use the same browser/organization authorization as native routes. */
export async function createServiceUserCredential(ctx: AppContext, auth: AuthContext, organizationId: string, serviceUserId: string, input: Create) {
  const owner = await serviceUserOwner(ctx, auth, organizationId, serviceUserId);
  return createCredential(ctx, auth, owner, credentialCreateSchema.parse(input));
}
export async function revokeServiceUserCredential(ctx: AppContext, auth: AuthContext, organizationId: string, serviceUserId: string, keyId: string) {
  return revokeCredential(ctx, auth, await serviceUserOwner(ctx, auth, organizationId, serviceUserId), keyId);
}
async function serviceUserOwner(ctx: AppContext, auth: AuthContext, organizationId: string, serviceUserId: string): Promise<Owner> {
  requireCredentialManagement(auth);
  await requireOrganizationAction(ctx, auth, organizationId, "manageServiceUsers");
  if (!z.string().uuid().safeParse(serviceUserId).success) throw new HTTPException(400, { message: "Invalid service user identifier" });
  const [app] = await ctx.db.select().from(schema.serviceUsers)
    .where(and(eq(schema.serviceUsers.id, serviceUserId), eq(schema.serviceUsers.organizationId, organizationId)));
  if (!app) throw new HTTPException(404, { message: "Service user not found" });
  return { serviceUserId, organizationId };
}

/** Every key that can reach an organization, for its administrators to audit:
 * its service users' keys, and the personal keys of people who hold a role in it
 * (members, by membership or policy, and Platform Admins) whose restrictions allow
 * it. Creating and revoking stay with each key's owner. */
async function listOrganizationCredentials(ctx: AppContext, auth: AuthContext, organizationId: string) {
  requireCredentialManagement(auth);
  const access = await requireOrganizationAction(ctx, auth, organizationId, "manageMembers");
  const organization = access.organization;
  const users = await ctx.db.select({ user: schema.user, membership: schema.member }).from(schema.user)
    .leftJoin(schema.member, and(eq(schema.member.userId, schema.user.id), eq(schema.member.organizationId, organizationId)));
  const holders = new Map(users.flatMap(({ user, membership }) => {
    const manual = ORGANIZATION_ROLES.find(role => role === membership?.role);
    const role = declaredPrincipalRole(organization.membershipPolicy, user, ORGANIZATION_ROLES, manual);
    return role || hasPlatformAdminRole(user) ? [[user.id, user] as const] : [];
  }));
  const universes = new Set((await ctx.db.select({ id: schema.universes.lightspeedUniverseId }).from(schema.universes)
    .where(eq(schema.universes.organizationId, organizationId))).map(universe => universe.id));
  const personal = holders.size ? await ctx.db.select({ ...publicKeyColumns, userId: schema.credentialBindings.userId }).from(schema.credentialBindings)
    .innerJoin(schema.apikey, eq(schema.apikey.id, schema.credentialBindings.keyId))
    .where(and(eq(schema.apikey.configId, PERSONAL_KEY_CONFIG), inArray(schema.credentialBindings.userId, [...holders.keys()]))) : [];
  const reaches = (restrictions: unknown) => {
    const limits = credentialRestrictionsSchema.parse(restrictions);
    if (limits.organizationIds && !limits.organizationIds.includes(organizationId)) return false;
    return !limits.universeIds || limits.universeIds.some(id => universes.has(id));
  };
  type KeyOwner = { kind: "user"; id: string; name: string; email: string } | { kind: "serviceUser"; id: string; name: string; enabled: boolean };
  const credentials: Array<ReturnType<typeof metadata> & { owner: KeyOwner; sortKey: Date }> = personal.filter(row => reaches(row.restrictions)).map(row => {
    const user = holders.get(row.userId!)!;
    return { ...metadata(row), owner: { kind: "user", id: user.id, name: user.name, email: user.email }, sortKey: row.createdAt };
  });
  if (access.actions.has("manageServiceUsers")) {
    const service = await ctx.db.select({ ...publicKeyColumns, serviceUserId: schema.serviceUsers.id, serviceUserName: schema.serviceUsers.name, serviceUserEnabled: schema.serviceUsers.enabled }).from(schema.credentialBindings)
      .innerJoin(schema.apikey, eq(schema.apikey.id, schema.credentialBindings.keyId))
      .innerJoin(schema.serviceUsers, eq(schema.serviceUsers.id, schema.credentialBindings.serviceUserId))
      .where(and(eq(schema.apikey.configId, SERVICE_USER_KEY_CONFIG), eq(schema.serviceUsers.organizationId, organizationId)));
    for (const row of service) credentials.push({ ...metadata(row), owner: { kind: "serviceUser", id: row.serviceUserId, name: row.serviceUserName, enabled: row.serviceUserEnabled }, sortKey: row.createdAt });
  }
  credentials.sort((left, right) => right.sortKey.getTime() - left.sortKey.getTime());
  return { credentials: credentials.map(({ sortKey: _, ...credential }) => credential) };
}

/** Mount at /api/v1/platform. Raw plugin management endpoints are never public. */
export function credentialRoutes(ctx: AppContext) {
  const routes = new Hono<{ Variables: ApiVariables }>();
  const personal = (auth: AuthContext): Owner => {
    requireCredentialManagement(auth);
    if (auth.principal.kind !== "user") throw new HTTPException(403, { message: "Personal credentials require a user" });
    return { userId: auth.principal.id };
  };
  const input = async (request: Request): Promise<Create> => {
    let raw: unknown;
    try { raw = await request.json(); } catch { throw new HTTPException(400, { message: "Invalid JSON" }); }
    const parsed = credentialCreateSchema.safeParse(raw);
    if (!parsed.success) throw new HTTPException(400, { message: "Invalid credential request" });
    return parsed.data;
  };
  const handlers: Record<keyof typeof CREDENTIAL_OPERATIONS, Handler<{ Variables: ApiVariables }>> = {
  "platform/credentials/list": async c => c.json(await listCredentials(ctx, personal(getAuth(c)))),
  "platform/credentials/create": async c => {
    const auth = getAuth(c); const owner = personal(auth);
    return c.json(await createCredential(ctx, auth, owner, await input(c.req.raw)), 201);
  },
  "platform/credentials/revoke": async c => {
    const auth = getAuth(c); return c.json(await revokeCredential(ctx, auth, personal(auth), c.req.param("keyId")!));
  },
  "platform/organizations/credentials/list": async c => c.json(await listOrganizationCredentials(ctx, getAuth(c), c.req.param("organizationId")!)),
  "platform/service-users/credentials/list": async c => c.json(await listCredentials(ctx, await serviceUserOwner(ctx, getAuth(c), c.req.param("organizationId")!, c.req.param("serviceUserId")!))),
  "platform/service-users/credentials/create": async c => {
    const auth = getAuth(c); const owner = await serviceUserOwner(ctx, auth, c.req.param("organizationId")!, c.req.param("serviceUserId")!);
    return c.json(await createCredential(ctx, auth, owner, await input(c.req.raw)), 201);
  },
  "platform/service-users/credentials/revoke": async c => {
    const auth = getAuth(c); const owner = await serviceUserOwner(ctx, auth, c.req.param("organizationId")!, c.req.param("serviceUserId")!);
    return c.json(await revokeCredential(ctx, auth, owner, c.req.param("keyId")!));
  },
  };
  const boundedBody = bodyLimit({ maxSize: 256 * 1024, onError: c => c.json({ type: "about:blank", title: "Payload too large", status: 413, kind: "invalidRequest", detail: "Credential request exceeds 256 KiB" }, 413, { "content-type": "application/problem+json" }) });
  for (const operation of Object.keys(CREDENTIAL_OPERATIONS) as (keyof typeof CREDENTIAL_OPERATIONS)[]) {
    const spec = CREDENTIAL_OPERATIONS[operation];
    const path = spec.path.replace(/^\/api\/v1\/platform/, "").replace(/\{([^}]+)\}/g, ":$1");
    routes.on(spec.method, path, boundedBody, handlers[operation]);
  }
  return routes;
}
