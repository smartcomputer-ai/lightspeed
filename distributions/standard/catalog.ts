import type { PlatformModule } from "@lightspeed-ai/platform-shared";
import { bots } from "../../modules/bots/module.js";
import { sessions } from "../../modules/sessions/module.js";
import { artifacts } from "../../modules/artifacts/module.js";

interface StandardModule extends PlatformModule {
  /** Optional UI package bundled into the shell. */
  web?: string;
  /** Trusted package or ESM entry. Its default export creates the module's API routes. */
  api?: string;
  /** Module-owned SQL migrations, relative to this distribution. */
  migrations?: string;
  demo?: string;
}

/** Used to assemble the web build and select the catalog offered by the API. */
export const standardModules: readonly StandardModule[] = [
  {
    ...bots,
    web: "@lightspeed-ai/bots-web",
    api: "@lightspeed-ai/bots-server/api",
    demo: "../../modules/bots/demo/index.ts",
  },
  {
    ...sessions,
    web: "@lightspeed-ai/sessions-web",
    api: "@lightspeed-ai/sessions-server/api",
    demo: "../../modules/sessions/demo/index.ts",
  },
  {
    ...artifacts,
    web: "@lightspeed-ai/artifacts-web",
    api: "@lightspeed-ai/artifacts-server/api",
    migrations: "../../modules/artifacts/db/migrations",
    demo: "../../modules/artifacts/demo/index.ts",
  },
];
