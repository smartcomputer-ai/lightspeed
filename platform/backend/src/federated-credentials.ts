import { isDeepStrictEqual } from "node:util";
import { and, eq, inArray, sql } from "drizzle-orm";
import { schema } from "@lightspeed-ai/platform-db";
import type { PlatformOperationParams } from "@lightspeed-ai/platform-shared";
import type { AppContext } from "./context.js";
import { requireCredentialManagement, type AuthContext } from "./auth-context.js";
import { organizationForPrincipal, requireOrganizationAction } from "./organization-access.js";
import { auditAsSubject } from "./identity-audit.js";
import { GateRefusal } from "./runtime-client.js";

const { serviceUsers, federatedCredentials: bindings } = schema;
type Owner = PlatformOperationParams<"platform/service-users/federated-credentials/list">;
type Put = PlatformOperationParams<"platform/service-users/federated-credentials/put">;
type Delete = PlatformOperationParams<"platform/service-users/federated-credentials/delete">;

function view(ctx: AppContext, binding: typeof bindings.$inferSelect) {
  const issuer = ctx.env.federation?.config.issuers.find((i) => i.issuer === binding.issuer);
  return {
    id: binding.id,
    issuer: issuer?.name ?? binding.issuer,
    subject: binding.subject,
    serviceAccountUid: binding.serviceAccountUid,
    restrictions: binding.restrictions,
    lastUsedAt: binding.lastUsedAt?.toISOString() ?? null,
  };
}

/** Include exportable declarations only for callers who can read credential inventory. */
export async function serviceUserDeclarations(
  ctx: AppContext,
  auth: AuthContext,
  organizationId: string,
  users: string[],
) {
  const result = new Map<string, Omit<ReturnType<typeof view>, "id" | "lastUsedAt">[]>();
  if (
    !users.length ||
    (auth.credential.restrictions.operations &&
      !auth.credential.restrictions.operations.includes("platform/service-users/federated-credentials/list"))
  )
    return result;
  const access = await organizationForPrincipal(ctx, auth, organizationId);
  if (!access?.actions.has("manageServiceUsers")) return result;
  const rows = await ctx.db
    .select()
    .from(bindings)
    .where(inArray(bindings.serviceUserId, users))
    .orderBy(bindings.issuer, bindings.subject);
  for (const row of rows) {
    const { issuer, subject, restrictions, serviceAccountUid } = view(ctx, row);
    const entries = result.get(row.serviceUserId) ?? [];
    entries.push({ issuer, subject, restrictions, serviceAccountUid });
    result.set(row.serviceUserId, entries);
  }
  return result;
}

async function requireOwner(ctx: AppContext, auth: AuthContext, owner: Owner) {
  await requireOrganizationAction(ctx, auth, owner.organizationId, "manageServiceUsers");
  const [user] = await ctx.db
    .select({ id: serviceUsers.id })
    .from(serviceUsers)
    .where(
      and(eq(serviceUsers.id, owner.serviceUserId), eq(serviceUsers.organizationId, owner.organizationId)),
    );
  if (!user) throw new GateRefusal(404, "Service user not found");
}

export async function listFederatedCredentials(ctx: AppContext, auth: AuthContext, owner: Owner) {
  await requireOwner(ctx, auth, owner);
  const rows = await ctx.db
    .select()
    .from(bindings)
    .where(eq(bindings.serviceUserId, owner.serviceUserId))
    .orderBy(bindings.issuer, bindings.subject);
  return {
    credentials: rows.map((row) => view(ctx, row)),
    issuers: ctx.env.federation?.config.issuers.map((i) => i.name) ?? [],
  };
}

function requireManagement(auth: AuthContext) {
  if (!auth.credential.restrictions.allowServiceUserKeys) requireCredentialManagement(auth);
}

export async function putFederatedCredential(ctx: AppContext, auth: AuthContext, input: Put) {
  requireManagement(auth);
  const issuer = ctx.env.federation?.config.issuers.find((i) => i.name === input.issuer)?.issuer;
  if (!issuer) throw new GateRefusal(400, "Unknown trusted issuer");
  if (/[\p{Cs}\u0000]/u.test(input.subject) || Buffer.byteLength(input.subject) > 1024)
    throw new GateRefusal(400, "Invalid workload subject");
  if (input.serviceAccountUid && /[\p{Cs}\u0000]/u.test(input.serviceAccountUid))
    throw new GateRefusal(400, "Invalid ServiceAccount UID");
  const restrictions = input.restrictions ?? {};
  const ceiling = auth.credential.restrictions;
  if (
    (ceiling.operations &&
      (!restrictions.operations ||
        restrictions.operations.some((op) => !ceiling.operations!.includes(op)))) ||
    (ceiling.allowUniverseCreation === false && restrictions.allowUniverseCreation !== false)
  )
    throw new GateRefusal(403, "Requested credential exceeds the caller's restrictions");
  return ctx.db.transaction(async (tx) => {
    await tx.execute(sql`SELECT pg_advisory_xact_lock(hashtextextended(${issuer}, 731845920115))`);
    await requireOwner({ ...ctx, db: tx as AppContext["db"] }, auth, input);
    const [current] = await tx
      .select()
      .from(bindings)
      .where(and(eq(bindings.issuer, issuer), eq(bindings.subject, input.subject)))
      .for("update");
    if (current && (input.createOnly || current.serviceUserId !== input.serviceUserId))
      throw new GateRefusal(409, "Workload already belongs to a service user");
    const values = { restrictions, serviceAccountUid: input.serviceAccountUid ?? null };
    if (
      current &&
      current.serviceAccountUid === values.serviceAccountUid &&
      isDeepStrictEqual(current.restrictions, restrictions)
    )
      return { credential: view(ctx, current) };
    const [binding] = current
      ? await tx.update(bindings).set(values).where(eq(bindings.id, current.id)).returning()
      : await tx
          .insert(bindings)
          .values({ ...values, issuer, subject: input.subject, serviceUserId: input.serviceUserId })
          .returning();
    await auditAsSubject(tx, auth, {
      action: "federatedCredential.put",
      targetId: binding!.id,
      details: {
        organizationId: input.organizationId,
        serviceUserId: input.serviceUserId,
        bindingIssuer: issuer,
        bindingSubject: input.subject,
      },
    });
    return { credential: view(ctx, binding!) };
  });
}

export async function deleteFederatedCredential(ctx: AppContext, auth: AuthContext, input: Delete) {
  requireManagement(auth);
  return ctx.db.transaction(async (tx) => {
    await requireOwner({ ...ctx, db: tx as AppContext["db"] }, auth, input);
    const [binding] = await tx
      .delete(bindings)
      .where(and(eq(bindings.serviceUserId, input.serviceUserId), eq(bindings.id, input.credentialId)))
      .returning();
    if (!binding) throw new GateRefusal(404, "Federated credential not found");
    await auditAsSubject(tx, auth, {
      action: "federatedCredential.delete",
      targetId: binding.id,
      details: {
        organizationId: input.organizationId,
        serviceUserId: input.serviceUserId,
        bindingIssuer: binding.issuer,
        bindingSubject: binding.subject,
      },
    });
    return { ok: true };
  });
}
