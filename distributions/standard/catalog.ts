import type { PlatformModule } from "@lightspeed-ai/platform-shared";
import { bots } from "../../modules/bots/module.js";
import { sessions } from "../../modules/sessions/module.js";
import { artifacts } from "../../modules/artifacts/module.js";

interface StandardModule extends PlatformModule {
  /** Optional UI package bundled into the shell. */
  web?: string;
  /** Trusted ESM entry relative to this distribution. Its default export creates the module's browser API. */
  api?: string;
  demo?: string;
}

/** Used to assemble the web build and select the catalog offered by the API. */
export const standardModules: readonly StandardModule[] = [
  {
    ...bots,
    web: "@lightspeed-ai/bots-web",
    api: "../../modules/bots/api/index.ts",
    demo: "../../modules/bots/demo/index.ts",
  },
  {
    ...sessions,
    web: "@lightspeed-ai/sessions-web",
    api: "../../modules/sessions/api/index.ts",
    demo: "../../modules/sessions/demo/index.ts",
  },
  { ...artifacts, web: "@lightspeed-ai/artifacts-web" },
];
