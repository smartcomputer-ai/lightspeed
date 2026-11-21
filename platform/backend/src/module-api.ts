import type { Hono } from "hono";
import type { ModuleRouteEntries, PlatformModule, UniverseAction } from "@lightspeed-ai/platform-shared";
import type { LightspeedClient } from "@lightspeed-ai/sdk";
import type { Db } from "@lightspeed-ai/platform-db";

export { parseBody } from "./http.js";
export { asManifest as asWorkspaceManifest } from "./vfs.js";

/** Trusted module repositories define their own tables on this PostgreSQL connection. */
export type ModuleDatabase = Pick<Db, "select" | "insert" | "update" | "delete" | "transaction" | "execute">;

export interface ModuleServices {
  db: ModuleDatabase;
}

/** Admitted scope, effective permissions and runtime transport for a trusted module route. */
export type UniverseModuleEnv = {
  Variables: {
    access: { organizationId: string; universeId: string; actorId: string; actions: ReadonlySet<UniverseAction> };
    permissions: ReadonlySet<string>;
    client: Pick<LightspeedClient, "call" | "openDownload">;
  };
};

export type ModuleRouteFactory = (services: ModuleServices) => Hono<UniverseModuleEnv>;

export interface ApiModule extends PlatformModule, ModuleRouteEntries<ModuleRouteFactory> {
  /** Absolute path to the selected module's Drizzle migration directory. */
  migrationsFolder?: string;
}
