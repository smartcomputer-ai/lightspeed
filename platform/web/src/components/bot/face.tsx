import { BotIcon } from "@/icons";
import { idColor } from "@/lib/identity-colors";
import { cn } from "@/lib/utils";

/// A bot keeps one colour everywhere it appears (roster, header, threads):
/// its id's colour, so renaming never changes the face.
export function botColor(botId: string): string {
  return idColor(botId);
}

/// A bot's avatar, wherever a bot is shown (its list, setup, creation, …):
/// its round face outlined in its colour. `size` is the face's diameter.
export function BotAvatar({
  botId,
  size = 24,
  className,
  color,
}: {
  botId: string;
  size?: number;
  className?: string;
  /** Override the derived colour (e.g. a bot with no id yet). */
  color?: string;
}) {
  return (
    <BotIcon
      size={size}
      // Cropped to the round body and its line (radius 8 on the 24 grid,
      // plus half the stroke), in a lighter line than the icon's, which
      // would be heavy at avatar sizes.
      viewBox="3.25 3.25 17.5 17.5"
      strokeWidth={1.5}
      className={cn("shrink-0", className)}
      // An avatar at its size, not an interface icon: the icon scale
      // (--icon-scale) does not apply.
      style={{ color: color ?? botColor(botId), zoom: 1 }}
      aria-hidden
    />
  );
}
