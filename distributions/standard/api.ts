import { withModuleRoutes } from "@lightspeed-ai/platform-shared";
import { botRoutes } from "../../modules/bots/api/index.js";
import { sessionCollectionRoutes } from "../../modules/sessions/api/index.js";
import { buildApp } from "../../platform/backend/src/api.js";
import type { AppContext } from "../../platform/backend/src/context.js";
import { standardModules } from "./catalog.js";

export const standardApiModules = withModuleRoutes(standardModules, { bots: botRoutes, sessions: sessionCollectionRoutes });

export function buildStandardApi(context: AppContext) {
  return buildApp(context, standardApiModules);
}
