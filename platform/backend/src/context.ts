import type { Db, DbHandle } from "@lightspeed-ai/platform-db";
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

export function isPlatformAdmin(session: Session): boolean {
  const role = session.user.role;
  return role !== null && role !== undefined && role.split(",").includes("admin");
}
