/// Screens captured from the demo build. Paths are relative to the demo base
/// (`/demo/`). Ids name the PNGs, so keep them stable: renaming one shows up
/// as a removed and an added screen in the next comparison.
///
/// Only plain routes for now; screens that need a click first (open dialogs,
/// expanded panels) come later as per-screen setup steps.
export interface Screen {
  id: string;
  path: string;
}

const factory = "u/software-factory";

export const SCREENS: Screen[] = [
  { id: "sessions", path: `${factory}/sessions` },
  { id: "session-detail", path: `${factory}/sessions/session-flaky-scheduler` },
  { id: "session-detail-audit", path: `${factory}/sessions/session-scheduler-audit` },
  { id: "bots", path: `${factory}/bots` },
  { id: "bot-chat", path: `${factory}/bots/planner` },
  { id: "bot-activity", path: `${factory}/bots/planner/activity` },
  { id: "bots-support", path: "u/technical-support/bots" },
  { id: "workspace", path: `${factory}/workspaces/acme-web` },
  { id: "workspace-file", path: `${factory}/workspaces/acme-web/files/docs/adr-0007.md` },
  { id: "profiles", path: `${factory}/profiles` },
  { id: "profile-detail", path: `${factory}/profiles/planner` },
  { id: "models", path: `${factory}/models` },
  { id: "environments", path: `${factory}/environments` },
  { id: "mcp-servers", path: `${factory}/mcp-servers` },
  { id: "credentials", path: `${factory}/credentials` },
  { id: "api-keys", path: `${factory}/api-keys` },
  { id: "members", path: `${factory}/members` },
  { id: "settings-general", path: `${factory}/settings/general` },
  { id: "settings-channels", path: `${factory}/settings/channels` },
  { id: "settings-templates", path: `${factory}/settings/templates` },
  { id: "admin-users", path: "admin/users" },
  { id: "admin-audit", path: "admin/audit" },
  { id: "admin-universes", path: "admin/universes" },
  { id: "admin-api-keys", path: "admin/api-keys" },
  { id: "admin-channels", path: "admin/channels" },
  { id: "admin-environment-providers", path: "admin/environment-providers" },
  { id: "account", path: "account" },
];

export const THEMES = ["light", "dark"] as const;
export type Theme = (typeof THEMES)[number];

export const VIEWPORT = { name: "desktop", width: 1440, height: 900, scale: 2 } as const;
