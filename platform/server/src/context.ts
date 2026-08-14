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
};

export function isPlatformAdmin(session: Session): boolean {
  const role = session.user.role;
  return role !== null && role !== undefined && role.split(",").includes("admin");
}
