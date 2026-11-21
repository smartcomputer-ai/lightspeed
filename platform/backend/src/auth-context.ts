import { HTTPException } from "hono/http-exception";
import type { Context } from "hono";
import type { schema } from "@lightspeed-ai/platform-db";
import type { CredentialRestrictions } from "@lightspeed-ai/platform-db/schema";
import type { ApiVariables } from "./context.js";
import { deploymentActions, type DeploymentAction } from "@lightspeed-ai/platform-shared";

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
    kind: "session" | "cliSession" | "personalKey" | "serviceUserKey" | "federated";
    id: string;
    restrictions: CredentialRestrictions;
    workload?: { issuer: string; subject: string };
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

interface CredentialTarget {
  operation: string; organizationId?: string; universeId?: string; write?: boolean; createUniverse?: boolean;
}

/** The same policy drives both server enforcement and caller-visible capabilities. */
export function credentialRefusal(auth: AuthContext, target: CredentialTarget): string | undefined {
  const limits = auth.credential.restrictions;
  if (limits.operations && !limits.operations.includes(target.operation)) {
    return "Credential does not permit this operation";
  }
  if (target.write && limits.universeIds && !target.universeId) return "Universe-limited credentials cannot perform organization-wide operations";
  if (target.write && limits.readOnly) return "Credential is read-only";
  if (target.organizationId && ((limits.organizationIds && !limits.organizationIds.includes(target.organizationId)) ||
      (auth.principal.kind === "serviceUser" && auth.principal.organizationId !== target.organizationId))) {
    return "Credential does not permit this organization";
  }
  if (target.universeId && limits.universeIds && !limits.universeIds.includes(target.universeId)) {
    return "Credential does not permit this universe";
  }
  if (target.createUniverse && (limits.allowUniverseCreation === false || limits.universeIds !== undefined)) {
    return "Credential does not permit universe creation";
  }
}

export function enforceRestrictions(auth: AuthContext, target: CredentialTarget): void {
  const reason = credentialRefusal(auth, target);
  if (reason) throw new AuthorizationRefusal(403, reason);
}

/** Restriction-bearing credentials cannot create, retarget, or widen credentials. */
export function requireCredentialManagement(auth: AuthContext): void {
  if (auth.principal.kind !== "user" || !["session", "cliSession"].includes(auth.credential.kind)) {
    throw new AuthorizationRefusal(403, "Manage credentials from an authenticated human session");
  }
}

/** Whether the principal holds a deployment action. Credential restrictions are enforced separately. */
export function deploymentAllows(auth: AuthContext, action: DeploymentAction): boolean {
  return deploymentActions(auth.principal.platformAdmin).has(action);
}
