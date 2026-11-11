import { z } from "zod";

/// The universe icons: illustrations, named after their design files.
export const UNIVERSE_ICONS = [
  "box-in-box", "personal-assistant", "network", "shield",
] as const;

/// What a universe without an icon of its own shows.
export const DEFAULT_UNIVERSE_ICON = "box-in-box" satisfies (typeof UNIVERSE_ICONS)[number];

/// Names of the earlier glyph icons. They stay valid so stored universes
/// still load (the database default is still `orbit`), and draw as the
/// default icon.
export const RETIRED_UNIVERSE_ICONS = [
  "orbit", "globe", "rocket", "star", "sparkles", "zap", "sun", "moon", "atom", "compass",
  "mountain", "leaf", "flame", "heart", "code", "briefcase", "house", "music", "bot",
  "anchor", "book", "camera", "coffee", "crown", "gem", "palette", "puzzle", "telescope", "cpu",
] as const;

export const UNIVERSE_ICON_COLORS = [
  "default", "slate", "red", "orange", "amber", "green", "teal", "blue", "violet", "pink",
] as const;

export const universeIconSchema = z.enum([...UNIVERSE_ICONS, ...RETIRED_UNIVERSE_ICONS]);
export const universeIconColorSchema = z.enum(UNIVERSE_ICON_COLORS);
export type UniverseIconName = z.infer<typeof universeIconSchema>;
export type OfferedUniverseIconName = (typeof UNIVERSE_ICONS)[number];
export type UniverseIconColor = z.infer<typeof universeIconColorSchema>;
