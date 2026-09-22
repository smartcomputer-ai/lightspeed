import { requestIdentity } from "./runtime-client.js";
import type { Db, DbHandle } from "@lightspeed/platform-db";
import type { Auth, Session } from "./auth.js";
import type { ServerEnv } from "./env.js";

export interface AppContext {
  db: Db;
  pool: DbHandle["pool"];
  auth: Auth;
  env: ServerEnv;
}

/// Hono context variables set by the session middleware.
export type ApiVariables = {
  session: Session;
  /// The session user's core principal, checked present by the API middleware.
  principalId: string;
};

export function isPlatformAdmin(): boolean {
  return requestIdentity.getStore()?.roles.includes("deployment_admin") ?? false;
}
