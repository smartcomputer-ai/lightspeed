import {
  DEFAULT_UNIVERSE_ICON,
  UNIVERSE_ICONS,
  type OfferedUniverseIconName,
  type UniverseIconName,
} from "@lightspeed-ai/platform-shared";

/// The universe icon illustrations: one 128px WebP per name in UNIVERSE_ICONS,
/// made from the design team's files of the same name.
const files = import.meta.glob<string>("../assets/universe-icons/*.webp", { eager: true, query: "?url", import: "default" });

export const UNIVERSE_ILLUSTRATIONS: Partial<Record<OfferedUniverseIconName, string>> = Object.fromEntries(
  Object.entries(files).map(([path, url]) => [path.slice(path.lastIndexOf("/") + 1, -".webp".length), url]),
);

/// The icon a stored name draws as: itself when offered, otherwise (no icon,
/// or a retired name) the default.
export function offeredUniverseIcon(icon: UniverseIconName | null | undefined): OfferedUniverseIconName {
  return (UNIVERSE_ICONS as readonly string[]).includes(icon ?? "")
    ? (icon as OfferedUniverseIconName)
    : DEFAULT_UNIVERSE_ICON;
}
