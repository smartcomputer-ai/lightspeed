import { UNIVERSE_ICON_CHOICES } from "@/icons";
import type { Ref } from "react";
import type { UniverseIconName, UniverseIconColor } from "@lightspeed-ai/platform-shared";
import { cn } from "@/lib/utils";
import { UNIVERSE_ICON_BACKGROUNDS } from "@/lib/identity-colors";

export function UniverseIcon({ icon = "orbit", iconColor = "default", className, ref }: {
  icon?: UniverseIconName;
  iconColor?: UniverseIconColor;
  className?: string;
  ref?: Ref<HTMLSpanElement>;
}) {
  const Icon = UNIVERSE_ICON_CHOICES[icon] ?? UNIVERSE_ICON_CHOICES.orbit;
  const background = UNIVERSE_ICON_BACKGROUNDS[iconColor] ?? UNIVERSE_ICON_BACKGROUNDS.default;
  const foreground = background === UNIVERSE_ICON_BACKGROUNDS.default
    ? "bg-sidebar-primary text-sidebar-primary-foreground"
    : "text-white";
  return (
    <span ref={ref} aria-hidden="true" style={{ background }} className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg", foreground, className)}>
      <Icon className="size-4" />
    </span>
  );
}
