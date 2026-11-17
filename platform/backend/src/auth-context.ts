import { HTTPException } from "hono/http-exception";
import type { Context } from "hono";
import type { schema } from "@lightspeed-ai/platform-db";
import type { CredentialRestrictions } from "@lightspeed-ai/platform-db/schema";
import type { ApiVariables } from "./context.js";

export type { CredentialRestrictions };
export type Principal = {
  kind: "user";
  id: string;
  user: typeof schema.user.$inferSelect;
  platformAdmin: boolean;
} | {
  kind: "serviceUser";
  id: string;
  serviceUser: typeof schema.serviceUsers.$inferSelect;
  organizationId: string;
  platformAdmin: false;
};

export interface AuthContext {
  principal: Principal;
  credential: {
    kind: "session" | "cliSession" | "personalKey" | "serviceUserKey";
    id: string;
    restrictions: CredentialRestrictions;
  };
  actorId: string;
  session?: { id: string; createdAt: Date; expiresAt: Date };
}

export class AuthorizationRefusal extends HTTPException {
  constructor(status: 401 | 403, message: string) { super(status, { message }); }
}

export function getAuth(c: Context<{ Variables: ApiVariables }>): AuthContext {
  const auth = c.get("auth");
  if (!auth) throw new AuthorizationRefusal(401, "Authentication required");
  return auth;
}

/** Credential ceilings always intersect current principal authority, including administrators. */
export function enforceRestrictions(auth: AuthContext, target: {
  operation: string; organizationId?: string; universeId?: string; write?: boolean; createUniverse?: boolean;
}): void {
  const limits = auth.credential.restrictions;
  if (limits.operations && !limits.operations.includes(target.operation)) {
    throw new AuthorizationRefusal(403, "Credential does not permit this operation");
  }
  if (target.write && limits.universeIds && !target.universeId) throw new AuthorizationRefusal(403, "Universe-limited credentials cannot perform organization-wide operations");
  if (target.write && limits.readOnly) throw new AuthorizationRefusal(403, "Credential is read-only");
  if (target.organizationId && ((limits.organizationIds && !limits.organizationIds.includes(target.organizationId)) ||
      (auth.principal.kind === "serviceUser" && auth.principal.organizationId !== target.organizationId))) {
    throw new AuthorizationRefusal(403, "Credential does not permit this organization");
  }
  if (target.universeId && limits.universeIds && !limits.universeIds.includes(target.universeId)) {
    throw new AuthorizationRefusal(403, "Credential does not permit this universe");
  }
  if (target.createUniverse && (limits.allowUniverseCreation === false || limits.universeIds !== undefined)) {
    throw new AuthorizationRefusal(403, "Credential does not permit universe creation");
  }
}

/** Restriction-bearing credentials cannot create, retarget, or widen credentials. */
export function requireCredentialManagement(auth: AuthContext): void {
  if (auth.principal.kind !== "user" || !["session", "cliSession"].includes(auth.credential.kind)) {
    throw new AuthorizationRefusal(403, "Manage credentials from an authenticated human session");
  }
}
