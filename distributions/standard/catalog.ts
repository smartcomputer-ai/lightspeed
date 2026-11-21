import type { ModuleRouteEntries, PlatformModule } from "@lightspeed-ai/platform-shared";
import { bots } from "../../modules/bots/module.js";
import { sessions } from "../../modules/sessions/module.js";
import { artifacts } from "../../modules/artifacts/module.js";

interface StandardModule extends PlatformModule {
  /** Optional UI package bundled into the shell. */
  web?: string;
  /** Trusted ESM entries, each exporting a default route factory for its surface. */
  api?: ModuleRouteEntries<string>;
  /** Module-owned SQL migrations, relative to this distribution. */
  migrations?: string;
  demo?: ModuleRouteEntries<string>;
}

/** Used to assemble the web build and select the catalog offered by the API. */
export const standardModules: readonly StandardModule[] = [
  {
    ...bots,
    web: "@lightspeed-ai/bots-web",
    api: { browserRoutes: "@lightspeed-ai/bots-server/api" },
    demo: { browserRoutes: "../../modules/bots/demo/index.ts" },
  },
  {
    ...sessions,
    web: "@lightspeed-ai/sessions-web",
    api: { browserRoutes: "@lightspeed-ai/sessions-server/api" },
    demo: { browserRoutes: "../../modules/sessions/demo/index.ts" },
  },
  {
    ...artifacts,
    web: "@lightspeed-ai/artifacts-web",
    api: { publicOperations: "@lightspeed-ai/artifacts-server/api" },
    migrations: "../../modules/artifacts/db/migrations",
    demo: { publicOperations: "../../modules/artifacts/demo/index.ts" },
  },
];
