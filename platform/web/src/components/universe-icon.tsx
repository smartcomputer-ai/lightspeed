import type { Ref } from "react";
import type { UniverseIconName, UniverseIconColor } from "@lightspeed-ai/platform-shared";
import { cn } from "@/lib/utils";
import { UNIVERSE_ICON_BACKGROUNDS } from "@/lib/identity-colors";
import { UNIVERSE_ILLUSTRATIONS, offeredUniverseIcon } from "@/components/universe-illustrations";

/// A universe's icon: a grayscale illustration, tinted by any color but the
/// default, keeping its shading. A retired or missing name draws the default.
export function UniverseIcon({ icon, iconColor = "default", className, ref }: {
  icon?: UniverseIconName | null;
  iconColor?: UniverseIconColor;
  className?: string;
  ref?: Ref<HTMLSpanElement>;
}) {
  const shown = offeredUniverseIcon(icon);
  const illustration = UNIVERSE_ILLUSTRATIONS[shown];
  const tint = iconColor === "default" ? undefined : UNIVERSE_ICON_BACKGROUNDS[iconColor];
  const mask = `url("${illustration}") center / contain no-repeat`;
  return (
    <span ref={ref} aria-hidden="true" className={cn("relative isolate flex size-8 shrink-0 items-center justify-center", className)}>
      <img src={illustration} alt="" draggable={false} data-universe-icon={shown} className="size-full object-contain" />
      {/* The color blend takes the hue and saturation of the tint and the
          lightness of the illustration; the mask keeps it to the icon's shape. */}
      {tint && (
        <span
          data-universe-icon-tint={iconColor}
          className="absolute inset-0 mix-blend-color"
          style={{ background: tint, mask, WebkitMask: mask }}
        />
      )}
    </span>
  );
}
