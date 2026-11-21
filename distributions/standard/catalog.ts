import { sessions } from "../../modules/sessions/module.js";
import { artifacts } from "../../modules/artifacts/module.js";

/** Used to assemble the web build and select the catalog offered by the API. */
export const standardModules = [sessions, artifacts] as const;
