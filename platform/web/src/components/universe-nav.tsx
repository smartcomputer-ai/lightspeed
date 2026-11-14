import type { ComponentType } from "react";
import { ApiKeyIcon, BotIcon, ChannelIcon, CredentialIcon, EnvironmentIcon, McpServerIcon, MemberIcon, ModelIcon, ProfileIcon, SessionIcon, SettingsIcon, TemplateIcon, WorkspaceIcon } from "@/icons";
import { Navigate } from "react-router-dom";
import type { FeatureKey, FeatureStates } from "@lightspeed-ai/platform-shared";
import { useActionPermissions, type PermissionAction } from "@/lib/permissions";
import { universeHome, useActiveUniverse } from "@/lib/universes";

/// One universe page in the sidebar. The item shows when the caller holds
/// `action` in the universe and its feature, if any, is on; the page applies
/// its own gate as well.
export interface UniverseNavItem {
  /// Relative to `/u/:handle/`.
  path: string;
  label: string;
  icon: ComponentType;
  action: PermissionAction;
  feature?: FeatureKey;
}

export function navItemVisible(
  item: UniverseNavItem,
  can: (action: PermissionAction) => boolean,
  features: FeatureStates | undefined,
): boolean {
  return can(item.action) && (!item.feature || features?.[item.feature] === true);
}

export interface UniverseNavGroup {
  label?: string;
  items: UniverseNavItem[];
}

/// The universe's own configuration, in the order `/settings` resolves.
const SETTINGS_NAV: UniverseNavItem[] = [
  { path: "settings/general", label: "General", icon: SettingsIcon, action: "manageAccess" },
  { path: "settings/channels", label: "Channels", icon: ChannelIcon, action: "configureResource", feature: "channels" },
  { path: "settings/templates", label: "Templates", icon: TemplateIcon, action: "configureResource" },
];

/// The work itself (with the workspaces that hold it), the setup agents are made from, access (credentials,
/// keys and members), and the universe's settings. A group renders only when
/// one of its items is visible.
export const UNIVERSE_NAV: UniverseNavGroup[] = [
  {
    // Unlabelled: the universe switcher above already names the universe.
    items: [
      { path: "bots", label: "Bots", icon: BotIcon, action: "read", feature: "bots" },
      { path: "sessions", label: "Sessions", icon: SessionIcon, action: "read" },
      { path: "workspaces", label: "Workspaces", icon: WorkspaceIcon, action: "read" },
    ],
  },
  {
    // What a session's setup attaches; a profile is the saved recipe.
    label: "Setup",
    items: [
      { path: "profiles", label: "Profiles", icon: ProfileIcon, action: "read" },
      { path: "environments", label: "Environments", icon: EnvironmentIcon, action: "read" },
      { path: "mcp-servers", label: "MCP servers", icon: McpServerIcon, action: "read" },
      { path: "models", label: "Models", icon: ModelIcon, action: "read" },
    ],
  },
  {
    label: "Access",
    items: [
      { path: "credentials", label: "Credentials", icon: CredentialIcon, action: "configureResource" },
      { path: "api-keys", label: "API keys", icon: ApiKeyIcon, action: "manageAccess" },
      { path: "members", label: "Members", icon: MemberIcon, action: "read" },
    ],
  },
  { label: "Settings", items: SETTINGS_NAV },
];

/// The sidebar's names for a universe page, most specific first, given the
/// path below `/u/:handle/`: ["Sessions"], or ["General", "Settings"] for a
/// page under its own group's path. Groups like Setup and Access are only
/// headings in the menu, not part of the path. Unknown pages yield [].
export function universeSectionTrail(path: string): string[] {
  for (const group of UNIVERSE_NAV) {
    const item = group.items.find((candidate) => path === candidate.path || path.startsWith(`${candidate.path}/`));
    if (item) return item.path.includes("/") && group.label ? [item.label, group.label] : [item.label];
  }
  return [];
}

/// Bare `/settings`: the first settings page the caller may see, else the
/// universe's home.
export function settingsIndexPath(
  universe: { handle: string; features?: FeatureStates },
  can: (action: PermissionAction) => boolean,
): string {
  const first = SETTINGS_NAV.find((item) => navItemVisible(item, can, universe.features));
  return first ? `/u/${universe.handle}/${first.path}` : universeHome(universe);
}

export function SettingsIndexRedirect() {
  const { universe } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (!universe || permissions.isLoading) {
    return null;
  }
  return <Navigate to={settingsIndexPath(universe, permissions.can)} replace />;
}
