import {
  Orbit, Globe, Rocket, Star, Sparkles, Zap, Sun, Moon, Atom, Compass,
  Mountain, Leaf, Flame, Heart, Code, Briefcase, House, Music, Shield, Bot, type LucideIcon,
  Anchor, Book, Camera, Coffee, Crown, Gem, Palette, Puzzle, Telescope, Cpu,
} from "lucide-react";
import type { UniverseIconName, UniverseIconColor } from "@lightspeed-ai/platform-shared";
import { cn } from "@/lib/utils";
import { UNIVERSE_ICON_BACKGROUNDS } from "@/lib/identity-colors";

const ICONS: Record<UniverseIconName, LucideIcon> = {
  orbit: Orbit, globe: Globe, rocket: Rocket, star: Star, sparkles: Sparkles,
  zap: Zap, sun: Sun, moon: Moon, atom: Atom, compass: Compass,
  mountain: Mountain, leaf: Leaf, flame: Flame, heart: Heart, code: Code, briefcase: Briefcase,
  house: House, music: Music, shield: Shield, bot: Bot,
  anchor: Anchor, book: Book, camera: Camera, coffee: Coffee, crown: Crown,
  gem: Gem, palette: Palette, puzzle: Puzzle, telescope: Telescope, cpu: Cpu,
};

export function UniverseIcon({ icon = "orbit", iconColor = "default", className }: {
  icon?: UniverseIconName;
  iconColor?: UniverseIconColor;
  className?: string;
}) {
  const Icon = ICONS[icon] ?? Orbit;
  const background = UNIVERSE_ICON_BACKGROUNDS[iconColor] ?? UNIVERSE_ICON_BACKGROUNDS.default;
  const foreground = background === UNIVERSE_ICON_BACKGROUNDS.default
    ? "bg-sidebar-primary text-sidebar-primary-foreground"
    : "text-white";
  return (
    <span aria-hidden="true" style={{ background }} className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg", foreground, className)}>
      <Icon className="size-4" />
    </span>
  );
}
