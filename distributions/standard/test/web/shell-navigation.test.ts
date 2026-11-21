import { expect, it } from "vitest";
import { standardWebModules } from "@distribution/web";
import { isMasterDetailRoute, isMobileDetailRoute } from "@/lib/shell-navigation";

it.each([
  "/u/team/sessions/session_1",
  "/u/team/sessions/session_1/",
  "/u/team/bots/bot_1",
  "/u/team/bots/bot_1/chat/session_1",
  "/u/team/bots/bot_1/activity",
  "/u/team/profiles/profile_1",
  "/u/team/workspaces/workspace_1/files/README.md",
  "/u/team/workspaces/workspace_1/files/src/main.rs",
])("hides the mobile shell header for detail %s", (path) => {
  expect(isMobileDetailRoute(path, standardWebModules)).toBe(true);
});
it.each([
  "/u/team/sessions", "/u/team/sessions/", "/u/team/bots",
  "/u/team/profiles", "/u/team/workspaces",
  "/u/team/workspaces/workspace_1", "/u/team/workspaces/workspace_1/files/",
  "/u/team/environments", "/admin/organizations/org/universes/team/settings", "/admin/platform/users", "/account",
])("keeps navigation for list or top-level page %s", (path) => {
  expect(isMobileDetailRoute(path, standardWebModules)).toBe(false);
});

it("uses the ordinary shell layout for detail URLs from omitted modules", () => {
  for (const path of ["/u/team/bots/bot/chat/session", "/u/team/sessions/session"]) {
    expect(isMobileDetailRoute(path, [])).toBe(false);
    expect(isMasterDetailRoute(path, [])).toBe(false);
  }
});
