import { z } from "zod";

/// The universe icons: illustrations, named after their design files.
export const UNIVERSE_ICONS = [
  "analytics", "approval", "authoring", "automation", "book", "callout", "conversation",
  "coverage", "data", "design", "dialogue", "energy", "evidence", "factory", "figures",
  "flight", "globe", "inventory", "jack", "journal", "layers", "load-block", "logic-gate",
  "logistics", "manufacturing", "marketing", "message", "mill", "mobius", "mold", "navigation",
  "network", "operations", "optimization", "personal-assistant", "process", "production",
  "quality-assurance", "research", "resilience", "risk-management", "safe", "sales", "sandbox",
  "security", "shield", "stack", "steps", "strategy", "support", "tool-chest", "verification",
  "writing",
] as const;

/// What a universe without an icon of its own shows.
export const DEFAULT_UNIVERSE_ICON = "layers" satisfies (typeof UNIVERSE_ICONS)[number];

/// Names of earlier icons that no illustration carries: the first glyphs
/// (the database default is still `orbit`) and the first illustrations.
/// They stay valid so stored universes still load and save; each draws as
/// its replacement, else as the default icon. An earlier name an
/// illustration now carries (`globe`, `book`, `shield`) simply draws it.
export const RETIRED_UNIVERSE_ICONS = [
  "orbit", "rocket", "star", "sparkles", "zap", "sun", "moon", "atom", "compass", "mountain",
  "leaf", "flame", "heart", "code", "briefcase", "house", "music", "bot", "anchor", "camera",
  "coffee", "crown", "gem", "palette", "puzzle", "telescope", "cpu", "box-in-box",
] as const;

export type OfferedUniverseIconName = (typeof UNIVERSE_ICONS)[number];
export type RetiredUniverseIconName = (typeof RETIRED_UNIVERSE_ICONS)[number];

/// The illustration each retired name draws as, where one fits it; the rest
/// draw as the default icon.
export const UNIVERSE_ICON_REPLACEMENTS: Partial<Record<RetiredUniverseIconName, OfferedUniverseIconName>> = {
  atom: "research",
  bot: "automation",
  briefcase: "tool-chest",
  code: "mobius",
  compass: "navigation",
  cpu: "logic-gate",
  flame: "energy",
  palette: "design",
  rocket: "flight",
  sparkles: "personal-assistant",
  telescope: "research",
  zap: "energy",
};

/// The illustration a stored icon name draws as: itself when offered, its
/// replacement when retired, otherwise (no icon, or no replacement) the
/// default.
export function currentUniverseIcon(icon: string | null | undefined): OfferedUniverseIconName {
  if ((UNIVERSE_ICONS as readonly string[]).includes(icon ?? "")) return icon as OfferedUniverseIconName;
  return UNIVERSE_ICON_REPLACEMENTS[icon as RetiredUniverseIconName] ?? DEFAULT_UNIVERSE_ICON;
}

export const UNIVERSE_ICON_COLORS = [
  "default", "slate", "red", "orange", "amber", "green", "teal", "blue", "violet", "pink",
] as const;

export const universeIconSchema = z.enum([...UNIVERSE_ICONS, ...RETIRED_UNIVERSE_ICONS]);
export const universeIconColorSchema = z.enum(UNIVERSE_ICON_COLORS);
export type UniverseIconName = z.infer<typeof universeIconSchema>;
export type UniverseIconColor = z.infer<typeof universeIconColorSchema>;
