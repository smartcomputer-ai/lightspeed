import { buildApp } from "../../platform/backend/src/api.js";
import type { AppContext } from "../../platform/backend/src/context.js";
import { standardModules } from "./catalog.js";

export function buildStandardApi(context: AppContext) {
  return buildApp(context, standardModules);
}
