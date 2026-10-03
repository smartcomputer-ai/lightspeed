import { z } from "zod";

export const UNIVERSE_ICONS = [
  "orbit", "globe", "rocket", "star", "sparkles", "zap", "sun", "moon",
  "atom", "compass", "mountain", "leaf", "flame", "heart", "code", "briefcase",
  "house", "music", "shield", "bot",
  "anchor", "book", "camera", "coffee", "crown", "gem", "palette", "puzzle", "telescope", "cpu",
] as const;
export const UNIVERSE_ICON_COLORS = [
  "default", "slate", "red", "orange", "amber", "green", "teal", "blue", "violet", "pink",
] as const;

export const universeIconSchema = z.enum(UNIVERSE_ICONS);
export const universeIconColorSchema = z.enum(UNIVERSE_ICON_COLORS);
export type UniverseIconName = z.infer<typeof universeIconSchema>;
export type UniverseIconColor = z.infer<typeof universeIconColorSchema>;
