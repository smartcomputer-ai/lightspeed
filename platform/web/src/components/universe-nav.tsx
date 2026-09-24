import type { ComponentType } from "react";
import type { UniverseAction } from "@lightspeed-ai/agent-client";
import {
  Boxes,
  BrainCircuit,
  FolderGit2,
  KeyRound,
  LockKeyhole,
  MessagesSquare,
  PackageOpen,
  RadioTower,
  Server,
  Settings,
  SlidersHorizontal,
  Users,
} from "lucide-react";
import { Navigate } from "react-router-dom";
import { BotFaceIcon } from "@/components/icons/bot";
import { useActionPermissions } from "@/lib/permissions";
import { universeHome, useActiveUniverse } from "@/lib/universes";

/// One universe page in the sidebar. The item shows when the caller holds
/// `action` in the universe; the page applies its own gate as well.
export interface UniverseNavItem {
  /// Relative to `/u/:slug/`.
  path: string;
  label: string;
  icon: ComponentType;
  action: UniverseAction;
}

export interface UniverseNavGroup {
  label?: string;
  items: UniverseNavItem[];
}

/// The universe's own configuration, in the order `/settings` resolves.
const SETTINGS_NAV: UniverseNavItem[] = [
  { path: "settings/general", label: "General", icon: Settings, action: "manage_access" },
  { path: "settings/channels", label: "Channels", icon: RadioTower, action: "configure_resource" },
  { path: "settings/templates", label: "Templates", icon: PackageOpen, action: "configure_resource" },
];

/// The work itself, the resources agents use, access (model providers,
/// credentials, keys and members), and the universe's settings. A group renders only when one of its items
/// is visible.
export const UNIVERSE_NAV: UniverseNavGroup[] = [
  {
    // Unlabelled: the universe switcher above already names the universe.
    items: [
      { path: "bots", label: "Bots", icon: BotFaceIcon, action: "read" },
      { path: "sessions", label: "Sessions", icon: MessagesSquare, action: "read" },
      { path: "profiles", label: "Profiles", icon: SlidersHorizontal, action: "read" },
      { path: "workspaces", label: "Workspaces", icon: FolderGit2, action: "read" },
    ],
  },
  {
    label: "Resources",
    items: [
      { path: "environments", label: "Environments", icon: Boxes, action: "read" },
      { path: "mcp-servers", label: "MCP servers", icon: Server, action: "read" },
    ],
  },
  {
    label: "Access",
    items: [
      { path: "models", label: "Models", icon: BrainCircuit, action: "read" },
      { path: "credentials", label: "Credentials", icon: LockKeyhole, action: "configure_resource" },
      { path: "api-keys", label: "API keys", icon: KeyRound, action: "read" },
      { path: "members", label: "Members", icon: Users, action: "read" },
    ],
  },
  { label: "Settings", items: SETTINGS_NAV },
];

/// Bare `/settings`: the first settings page the caller may see, else the
/// universe's home.
export function settingsIndexPath(
  slug: string,
  can: (action: UniverseAction) => boolean,
): string {
  const first = SETTINGS_NAV.find((item) => can(item.action));
  return first ? `/u/${slug}/${first.path}` : universeHome(slug);
}

export function SettingsIndexRedirect() {
  const { universe } = useActiveUniverse();
  const permissions = useActionPermissions(universe?.id);
  if (!universe || permissions.isLoading) {
    return null;
  }
  return <Navigate to={settingsIndexPath(universe.slug, permissions.can)} replace />;
}
