import type { Db, DbHandle } from "@lightspeed-ai/platform-db";
import { deploymentActions, type DeploymentAction } from "@lightspeed-ai/platform-shared";
import type { Auth, Session } from "./auth.js";
import type { AuthContext } from "./auth-context.js";
import type { ServerEnv } from "./env.js";

export interface AppContext {
  db: Db;
  pool: DbHandle["pool"];
  auth: Auth;
  env: ServerEnv;
}

/// Hono context variables set by the session middleware.
export type ApiVariables = {
  auth: AuthContext;
  /** Set only by browser session admission; API-key requests never synthesize it. */
  session: Session;
};

/** Whether a user record holds the Platform Admin role. */
export function hasPlatformAdminRole(user: { role?: string | null }): boolean {
  return user.role?.split(",").includes("admin") === true;
}

/** Whether a browser session's user holds a deployment action. */
export function sessionAllows(session: Session, action: DeploymentAction): boolean {
  return deploymentActions(hasPlatformAdminRole(session.user)).has(action);
}
