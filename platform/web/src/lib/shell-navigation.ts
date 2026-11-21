import { matchPath } from "react-router-dom";
import type { UniverseWebModule } from "./web-modules";

/** Detail panes replace their section's list on mobile. */
export function isMobileDetailRoute(pathname: string, modules: readonly UniverseWebModule[]): boolean {
  return modules.some(module => module.routes.some(route => route.mobileDetail && matchPath(`/u/:handle/${module.id}/${route.path}`, pathname)))
    || Boolean(matchPath("/u/:handle/profiles/:profileId/*", pathname))
    || Boolean(matchPath("/u/:handle/workspaces/:workspaceId/files/*", pathname)?.params["*"]);
}

export function isMasterDetailRoute(pathname: string, modules: readonly UniverseWebModule[]): boolean {
  const section = matchPath("/u/:handle/:section/*", pathname)?.params.section;
  return !!section && (["profiles", "workspaces"].includes(section)
    || modules.some(module => module.id === section && module.masterDetail));
}
