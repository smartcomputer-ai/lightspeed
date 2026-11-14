import type { UniverseIconColor } from "@lightspeed-ai/platform-shared";

/// Colours for things that have an identity. Two kinds:
///
/// - From an id (`idColor`): a stable hue hashed from an immutable id, so a
///   thing keeps its colour everywhere and when renamed. Bots use it. The
///   theme sets its lightness and chroma (--id-color-lightness,
///   --id-color-chroma): deep in the light theme, bright in the dark one.
/// - Picked (`identityColor`): a fixed hue at a badge's lightness, for
///   fills such as a universe icon's background.

/// The hue (0–359) an id always gets.
export function idHue(id: string): number {
  let hash = 7;
  for (const char of id) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return hash % 360;
}

/// A hue at the theme's id-colour lightness and chroma.
export function idHueColor(hue: number): string {
  return `oklch(var(--id-color-lightness) var(--id-color-chroma) ${hue})`;
}

/// The colour an id keeps everywhere it appears.
export function idColor(id: string): string {
  return idHueColor(idHue(id));
}

/// Shared lightness and saturation keep identity badges visually consistent.
export function identityColor(hue: number, chroma = 0.11): string {
  return `oklch(0.58 ${chroma} ${hue})`;
}

export const UNIVERSE_ICON_BACKGROUNDS: Record<UniverseIconColor, string> = {
  default: "var(--sidebar-primary)",
  slate: identityColor(260, 0.02),
  red: identityColor(25),
  orange: identityColor(55),
  amber: identityColor(85),
  green: identityColor(145),
  teal: identityColor(185),
  blue: identityColor(255),
  violet: identityColor(300),
  pink: identityColor(345),
};
