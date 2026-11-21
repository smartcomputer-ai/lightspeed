import type { ComponentType } from "react";
import { BotIcon, CredentialIcon, EnvironmentIcon, McpServerIcon, MemberIcon, ModelIcon, ProfileIcon, SettingsIcon, WorkspaceIcon } from "@/icons";
import { Navigate } from "react-router-dom";
import type { FeatureKey, FeatureStates } from "@lightspeed-ai/platform-shared";
import type { UniverseAction } from "@lightspeed-ai/platform-shared";
import type { UniverseWebModule } from "@/lib/web-modules";
import { useActionPermissions } from "@/lib/permissions";
import { universeHome, useActiveUniverse } from "@/lib/universes";

/// One universe page in the sidebar. The item shows when the caller holds
/// `action` in the universe and its feature, if any, is on; the page applies
/// its own gate as well.
export interface UniverseNavItem {
  /// Relative to `/u/:handle/`.
  path: string;
  label: string;
  icon: ComponentType;
  action: UniverseAction;
  feature?: FeatureKey;
}

export function navItemVisible(
  item: UniverseNavItem,
  can: (action: UniverseAction) => boolean,
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
  { path: "settings/general", label: "General", icon: SettingsIcon, action: "administer" },
  { path: "settings/members", label: "Members", icon: MemberIcon, action: "administer" },
];

/// The work itself, the setup agents use (including outbound credentials),
/// and the universe's access and settings. A group renders only when
/// one of its items is visible.
export function universeNavigation(modules: readonly UniverseWebModule[]): UniverseNavGroup[] {
  return [
    {
      // Unlabelled: the universe switcher above already names the universe.
      items: [
        { path: "bots", label: "Bots", icon: BotIcon, action: "read", feature: "bots" },
        ...modules.map(({ id, icon, label, action }) => ({ path: id, icon, label, action })),
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
        { path: "credentials", label: "Credentials", icon: CredentialIcon, action: "configureResource" },
      ],
    },
    {
      label: "Settings",
      items: [
        ...SETTINGS_NAV,
      ],
    },
  ];
}

/// The sidebar's names for a universe page, most specific first, given the
/// path below `/u/:handle/`: ["Sessions"], or ["General", "Settings"] for a
/// page under its own group's path. Groups like Setup are only
/// headings in the menu, not part of the path. Unknown pages yield [].
export function universeSectionTrail(path: string, groups: UniverseNavGroup[]): string[] {
  for (const group of groups) {
    const item = group.items.find((candidate) => path === candidate.path || path.startsWith(`${candidate.path}/`));
    if (item) return item.path.includes("/") && group.label ? [item.label, group.label] : [item.label];
  }
  return [];
}

/// Bare `/settings`: the first settings page the caller may see, else the
/// universe's home.
export function settingsIndexPath(
  universe: { handle: string; features?: FeatureStates },
  can: (action: UniverseAction) => boolean,
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
