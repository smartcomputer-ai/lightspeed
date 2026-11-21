import { composeWebModules } from "@/lib/web-modules";
import entries from "virtual:lightspeed/web-module-entries";

/** Vite imports exactly the native module entry points selected by this distribution's catalog. */
export const standardWebModules = composeWebModules(entries);
