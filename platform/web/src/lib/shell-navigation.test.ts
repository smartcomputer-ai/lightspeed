import { expect, it } from "vitest";
import { isMobileDetailRoute } from "./shell-navigation";

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
  expect(isMobileDetailRoute(path)).toBe(true);
});
it.each([
  "/u/team/sessions", "/u/team/sessions/", "/u/team/bots",
  "/u/team/profiles", "/u/team/workspaces",
  "/u/team/workspaces/workspace_1", "/u/team/workspaces/workspace_1/files/",
  "/u/team/settings/environments", "/admin/users", "/account",
])("keeps navigation for list or top-level page %s", (path) => {
  expect(isMobileDetailRoute(path)).toBe(false);
});
