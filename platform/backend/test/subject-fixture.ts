import type { AuthContext } from "../src/auth-context.js";
import type { schema } from "@lightspeed-ai/platform-db";

export function userSubject(id = "caller", platformAdmin = false, user: Partial<typeof schema.user.$inferSelect> = {}): AuthContext {
  return { principal: { kind: "user", id, platformAdmin, user: { id, name: id, email: `${id}@example.test`, emailVerified: true, banned: false, identitySource: "local", companyAdmitted: false, directoryGroups: [], role: platformAdmin ? "admin" : "user", ...user } as typeof schema.user.$inferSelect }, actorId: id, credential: { kind: "session", id: `session-${id}`, restrictions: {} } };
}

export function serviceUserSubject(serviceUser: typeof schema.serviceUsers.$inferSelect): AuthContext {
  return { principal: { kind: "serviceUser", id: serviceUser.id, serviceUser, organizationId: serviceUser.organizationId, platformAdmin: false }, actorId: `serviceUser:${serviceUser.id}`, credential: { kind: "serviceUserKey", id: `key-${serviceUser.id}`, restrictions: {} } };
}
