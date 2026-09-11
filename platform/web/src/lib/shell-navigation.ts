import { matchPath } from "react-router-dom";

/** Match the detail panes that replace their section's list on mobile. */
export function isMobileDetailRoute(pathname: string): boolean {
  return [
    "/u/:slug/sessions/:sessionId/*",
    "/u/:slug/bots/:botId/*",
    "/u/:slug/profiles/:profileId/*",
  ].some((path) => matchPath(path, pathname) !== null)
    || Boolean(matchPath("/u/:slug/workspaces/:workspaceId/files/*", pathname)?.params["*"]);
}
