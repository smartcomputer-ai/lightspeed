import { Hono } from "hono";
import { bodyLimit } from "hono/body-limit";
import { HTTPException } from "hono/http-exception";
import { and, eq, sql } from "drizzle-orm";
import { schema, type Db } from "@lightspeed-ai/platform-db";
import { PLATFORM_OPERATIONS, type PlatformOperationParams } from "@lightspeed-ai/platform-shared";
import { enforceRestrictions } from "../auth-context.js";
import { resolveFederatedBinding } from "../federated-binding.js";
import type { WorkloadFederation, WorkloadIdentity } from "../federation.js";
import { auditIdentity } from "../identity-audit.js";
import { problem } from "../runtime-http.js";

const operation = "platform/organizations/enroll";
const spec = PLATFORM_OPERATIONS[operation];

/** Match the full subject. Only '*' is special, and all other characters are literal. */
function matchesSubject(pattern: string, subject: string) {
  const [prefix, ...parts] = pattern.split("*");
  if (!parts.length) return subject === prefix;
  const suffix = parts.pop()!;
  if (!subject.startsWith(prefix!) || !subject.endsWith(suffix)) return false;
  let offset = prefix!.length;
  for (const part of parts) {
    const index = subject.indexOf(part, offset);
    if (index < 0) return false;
    offset = index + part.length;
  }
  return offset <= subject.length - suffix.length;
}

async function enroll(db: Db, federation: WorkloadFederation, identity: WorkloadIdentity, input: PlatformOperationParams<typeof operation>) {
  const { organization, organizationEnrollments, serviceUsers, federatedCredentials } = schema;
  const { issuer, subject } = identity;
  return db.transaction(async tx => {
    // Serialize first contact across replicas before checking either the binding or the remaining quota.
    await tx.execute(sql`SELECT pg_advisory_xact_lock(hashtextextended(${issuer}, 731845920115))`);
    const binding = await resolveFederatedBinding(tx, identity);
    if (binding) {
      if (!binding.auth) throw new HTTPException(401, { message: "Workload credential is inactive" });
      if (binding.handle !== input.handle) throw new HTTPException(409, { message: "Workload is bound to another organization" });
      enforceRestrictions(binding.auth, { operation, organizationId: binding.organizationId });
      return { organizationId: binding.organizationId, enrolled: false };
    }

    const issuerName = federation.config.issuers.find(i => i.issuer === issuer)?.name;
    const rule = federation.config.enrollment.find(r => r.issuer === issuerName && matchesSubject(r.subjectPattern, subject));
    if (!rule) throw new HTTPException(403, { message: "No enrollment rule matches this workload" });
    if (rule.handlePattern && !rule.handlePattern.test(input.handle)) throw new HTTPException(403, { message: "Handle is outside the enrollment rule" });
    const [previous] = await tx.select({ id: organizationEnrollments.organizationId }).from(organizationEnrollments)
      .where(and(eq(organizationEnrollments.issuer, issuer), eq(organizationEnrollments.subject, subject)));
    if (previous) throw new HTTPException(409, { message: "Workload already enrolled an organization" });
    const total = await tx.$count(organizationEnrollments,
      and(eq(organizationEnrollments.issuer, issuer), eq(organizationEnrollments.subjectPattern, rule.subjectPattern)));
    if (total >= rule.maxOrganizations) throw new HTTPException(409, { message: "Enrollment rule has reached its organization limit" });

    const organizationId = crypto.randomUUID();
    const [created] = await tx.insert(organization).values({ id: organizationId, name: input.name, slug: input.handle,
      createdAt: new Date(), universeQuota: rule.organization.universeQuota }).onConflictDoNothing({ target: organization.slug }).returning({ id: organization.id });
    if (!created) throw new HTTPException(409, { message: "Organization handle is taken" });
    const [user] = await tx.insert(serviceUsers).values({ organizationId, ...rule.serviceUser }).returning({ id: serviceUsers.id });
    const [credential] = await tx.insert(federatedCredentials).values({ issuer, subject, serviceUserId: user!.id, lastUsedAt: new Date() })
      .onConflictDoNothing({ target: [federatedCredentials.issuer, federatedCredentials.subject] }).returning({ id: federatedCredentials.id });
    if (!credential) throw new HTTPException(409, { message: "Workload already has a credential binding" });
    await tx.insert(organizationEnrollments).values({ organizationId, issuer, subject, subjectPattern: rule.subjectPattern });
    await auditIdentity(tx, { action: "organization.enrolled", targetId: organizationId, actorId: `serviceUser:${user!.id}`,
      details: { issuer, subject, subjectPattern: rule.subjectPattern, credentialKind: "federated", credentialId: credential.id } });
    return { organizationId, enrolled: true };
  });
}

/** Only this route accepts a verified identity that has no Platform principal yet. */
export function enrollmentRoutes(db: Db, federation: WorkloadFederation) {
  const app = new Hono();
  app.onError((error, c) => {
    if (!(error instanceof HTTPException)) throw error;
    const kind = { 400: "invalidRequest", 401: "unauthenticated", 403: "forbidden", 409: "conflict" }[error.status as number];
    return problem(c, error.status, kind ?? "invalidRequest", error.message);
  });
  app.post(spec.path, bodyLimit({ maxSize: 16 * 1024, onError: c => problem(c, 413, "invalidRequest", "Enrollment body too large") }), async c => {
    const token = /^Bearer ([^\s,]+)$/i.exec(c.req.header("authorization") ?? "")?.[1];
    const identity = token && await federation.verify(token);
    if (!identity) return problem(c, 401, "unauthenticated", "Verified workload token required");
    const input = spec.params.safeParse(await c.req.json().catch(() => null));
    if (!input.success) return problem(c, 400, "invalidRequest", "A valid organization handle and name are required");
    const result = await enroll(db, federation, identity, input.data);
    return c.json(result, result.enrolled ? 201 : 200);
  });
  return app;
}
