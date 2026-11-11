import { matchPath } from "react-router-dom";

/** Match the detail panes that replace their section's list on mobile. */
export function isMobileDetailRoute(pathname: string): boolean {
  return [
    "/u/:handle/sessions/:sessionId/*",
    "/u/:handle/bots/:botId/*",
    "/u/:handle/profiles/:profileId/*",
  ].some((path) => matchPath(path, pathname) !== null)
    || Boolean(matchPath("/u/:handle/workspaces/:workspaceId/files/*", pathname)?.params["*"]);
}
