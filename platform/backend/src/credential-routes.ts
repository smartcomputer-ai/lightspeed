import { and, desc, eq, inArray } from "drizzle-orm";
import { Hono, type Handler } from "hono";
import { bodyLimit } from "hono/body-limit";
import { HTTPException } from "hono/http-exception";
import { schema } from "@lightspeed-ai/platform-db";
import { CREDENTIAL_OPERATIONS, ORGANIZATION_ROLES, credentialCreateSchema, credentialPutSchema, credentialRestrictionsSchema } from "@lightspeed-ai/platform-shared";
import { defaultKeyHasher } from "@better-auth/api-key";
import { z } from "zod";
import type { ApiVariables, AppContext } from "./context.js";
import { enforceRestrictions, getAuth, requireCredentialManagement, type AuthContext } from "./auth-context.js";
import { declaredPrincipalRole, requireOrganizationAction } from "./organization-access.js";
import { hasPlatformAdminRole } from "./context.js";
import { PERSONAL_KEY_CONFIG, PERSONAL_KEY_PREFIX, SERVICE_USER_KEY_CONFIG, SERVICE_USER_KEY_PREFIX, apiKeyRowDefaults } from "./api-key-config.js";
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

const SECRET_ALPHABET = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
/** A new secret in the form Better Auth issues: the prefix and 64 letters. */
function newSecret(prefix: string): string {
  const letters: string[] = [];
  // Rejection sampling keeps every letter equally likely.
  while (letters.length < 64) {
    for (const byte of crypto.getRandomValues(new Uint8Array(96))) {
      if (byte < 208 && letters.length < 64) letters.push(SECRET_ALPHABET[byte % 52]!);
    }
  }
  return prefix + letters.join("");
}

/** The secret's columns as Better Auth stores them: a hash it finds the key
 * by, and the first characters it shows. */
async function secretColumns(secret: string) {
  return { key: await defaultKeyHasher(secret), start: secret.slice(0, 12) };
}

/** Give a key a new secret in place: its ID, name, restrictions and expiry
 * stay, the old secret stops working at once, and the new one is returned
 * once. Nothing that holds the key needs renaming. */
async function replaceCredential(ctx: AppContext, auth: AuthContext, owner: Owner, keyId: string) {
  const [row] = await ctx.db.select(publicKeyColumns).from(schema.credentialBindings)
    .innerJoin(schema.apikey, eq(schema.apikey.id, schema.credentialBindings.keyId))
    .where(and(ownerConditions(owner), eq(schema.apikey.id, keyId)));
  if (!row) throw new HTTPException(404, { message: "Credential not found" });
  if (!row.enabled) throw new HTTPException(409, { message: "A revoked key cannot be given a new secret" });
  const secret = newSecret(owner.serviceUserId ? SERVICE_USER_KEY_PREFIX : PERSONAL_KEY_PREFIX);
  const columns = await secretColumns(secret);
  await ctx.db.transaction(async (tx) => {
    await tx.update(schema.apikey).set({ ...columns, updatedAt: new Date() }).where(eq(schema.apikey.id, keyId));
    await auditIdentity(tx, { actorId: auth.actorId, action: "credential.replace", targetId: keyId,
      details: { principalKind: owner.serviceUserId ? "serviceUser" : "user", principalId: owner.serviceUserId ?? owner.userId!, credentialId: auth.credential.id } });
  });
  return { credential: metadata({ ...row, start: columns.start }), secret };
}

/** Declare a service user's key by name with a supplied secret: created when
 * missing; otherwise its secret, restrictions and expiry are made to match.
 * The comparison happens here, so a dry run tells a client what would change
 * without the client ever seeing a stored secret. */
async function putServiceUserCredential(ctx: AppContext, auth: AuthContext, owner: Owner & { serviceUserId: string }, input: z.infer<typeof credentialPutSchema>) {
  const maximum = ctx.env.apiKeyMaxExpiryDays;
  const expiresAt = input.expiresAt ? new Date(input.expiresAt) : null;
  if (maximum !== undefined && (expiresAt === null || expiresAt.getTime() > Date.now() + maximum * 86400_000)) {
    throw new HTTPException(400, { message: `This deployment requires keys to expire within ${maximum} days` });
  }
  const restrictions = credentialRestrictionsSchema.parse(input.restrictions);
  const columns = await secretColumns(input.secret);
  const named = await ctx.db.select({ ...publicKeyColumns, key: schema.apikey.key }).from(schema.credentialBindings)
    .innerJoin(schema.apikey, eq(schema.apikey.id, schema.credentialBindings.keyId))
    .where(and(ownerConditions(owner), eq(schema.apikey.name, input.name), eq(schema.apikey.enabled, true)));
  if (named.length > 1) throw new HTTPException(409, { message: `The service user has several keys named ${input.name}; revoke all but one` });
  const current = named[0];
  // Better Auth finds a key by its secret's hash, so a secret serves one key.
  const [taken] = await ctx.db.select({ id: schema.apikey.id }).from(schema.apikey).where(eq(schema.apikey.key, columns.key));
  if (taken && taken.id !== current?.id) throw new HTTPException(409, { message: "This secret is already used by another key" });
  const secretReplaced = !!current && current.key !== columns.key;
  const updated: ("restrictions" | "expiresAt")[] = [];
  if (current && JSON.stringify(credentialRestrictionsSchema.parse(current.restrictions)) !== JSON.stringify(restrictions)) updated.push("restrictions");
  if (current && (current.expiresAt?.getTime() ?? null) !== (expiresAt?.getTime() ?? null)) updated.push("expiresAt");
  const outcome = { created: !current, secretReplaced, updated };
  if (input.dryRun) return { credential: current ? metadata(current) : null, ...outcome };
  // Which credential declared the key: a person's session, or a key created to declare keys.
  const details = { principalKind: "serviceUser", principalId: owner.serviceUserId, credentialId: auth.credential.id, credentialKind: auth.credential.kind };
  const keyId = await ctx.db.transaction(async (tx) => {
    if (!current) {
      const id = crypto.randomUUID();
      const now = new Date();
      await tx.insert(schema.apikey).values({ ...apiKeyRowDefaults(), ...columns, id, configId: SERVICE_USER_KEY_CONFIG, name: input.name,
        prefix: SERVICE_USER_KEY_PREFIX, referenceId: owner.organizationId, expiresAt, createdAt: now, updatedAt: now });
      await tx.insert(schema.credentialBindings).values({ keyId: id, serviceUserId: owner.serviceUserId, restrictions, createdBy: auth.actorId });
      await auditIdentity(tx, { actorId: auth.actorId, action: "credential.put", targetId: id, details: { ...details, created: true } });
      return id;
    }
    if (secretReplaced || updated.length) {
      await tx.update(schema.apikey).set({ ...(secretReplaced ? columns : {}), expiresAt, updatedAt: new Date() }).where(eq(schema.apikey.id, current.id));
      if (updated.includes("restrictions")) await tx.update(schema.credentialBindings).set({ restrictions }).where(eq(schema.credentialBindings.keyId, current.id));
      await auditIdentity(tx, { actorId: auth.actorId, action: "credential.put", targetId: current.id, details: { ...details, secretReplaced, updated: updated.join(",") } });
    }
    return current.id;
  });
  const [row] = await ctx.db.select(publicKeyColumns).from(schema.credentialBindings)
    .innerJoin(schema.apikey, eq(schema.apikey.id, schema.credentialBindings.keyId)).where(eq(schema.apikey.id, keyId));
  return { credential: metadata(row!), ...outcome };
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

/** Who may declare a service user's key with a supplied secret: a person's
 * session, or a key created to declare them (`allowServiceUserKeys`), within
 * its other restrictions. Every other key operation stays with people, so an
 * ordinary key that leaks can mint nothing. */
function requireKeyDeclaration(auth: AuthContext, organizationId: string) {
  if (auth.principal.kind === "user" && ["session", "cliSession"].includes(auth.credential.kind)) return;
  if (auth.credential.restrictions.allowServiceUserKeys !== true) {
    throw new HTTPException(403, { message: "Declare service-user keys from a human session, or with a key created to declare them (allowServiceUserKeys)" });
  }
  enforceRestrictions(auth, { operation: "platform/service-users/credentials/put", organizationId, write: true });
}

async function serviceUserOwner(ctx: AppContext, auth: AuthContext, organizationId: string, serviceUserId: string, declaring = false): Promise<Owner> {
  if (declaring) requireKeyDeclaration(auth, organizationId);
  else requireCredentialManagement(auth);
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
  "platform/credentials/replace": async c => {
    const auth = getAuth(c); return c.json(await replaceCredential(ctx, auth, personal(auth), c.req.param("keyId")!));
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
  "platform/service-users/credentials/replace": async c => {
    const auth = getAuth(c); const owner = await serviceUserOwner(ctx, auth, c.req.param("organizationId")!, c.req.param("serviceUserId")!);
    return c.json(await replaceCredential(ctx, auth, owner, c.req.param("keyId")!));
  },
  "platform/service-users/credentials/put": async c => {
    const auth = getAuth(c); const owner = await serviceUserOwner(ctx, auth, c.req.param("organizationId")!, c.req.param("serviceUserId")!, true);
    let raw: unknown;
    try { raw = await c.req.json(); } catch { throw new HTTPException(400, { message: "Invalid JSON" }); }
    const parsed = credentialPutSchema.safeParse(raw);
    if (!parsed.success) throw new HTTPException(400, { message: `Invalid credential: ${parsed.error.issues.map(issue => `${issue.path.join(".")}: ${issue.message}`).join("; ")}` });
    return c.json(await putServiceUserCredential(ctx, auth, owner as Owner & { serviceUserId: string }, parsed.data));
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
