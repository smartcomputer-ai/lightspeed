import type { UniverseIconColor } from "@lightspeed/platform-shared";

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
