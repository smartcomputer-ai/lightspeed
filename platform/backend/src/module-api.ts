import type { Hono } from "hono";
import type { PlatformModule, UniverseAction } from "@lightspeed-ai/platform-shared";
import type { LightspeedClient } from "@lightspeed-ai/sdk";

export { parseBody } from "./http.js";

/** Host-provided context for a trusted module's private browser endpoints. */
export type UniverseModuleEnv = {
  Variables: {
    access: { organizationId: string; universeId: string; actorId: string; actions: ReadonlySet<UniverseAction> };
    client: Pick<LightspeedClient, "call">;
  };
};

export interface ApiModule extends PlatformModule {
  routes?: () => Hono<UniverseModuleEnv>;
}
