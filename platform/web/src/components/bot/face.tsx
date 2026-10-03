import { BotFace } from "@/components/icons/bot";
import { cn } from "@/lib/utils";
import { identityColor } from "@/lib/identity-colors";

/// A bot keeps one colour everywhere it appears — roster, header, threads —
/// derived from its immutable id, so renaming never changes the face.
export function botHue(botId: string): number {
  let hash = 7;
  for (const char of botId) hash = (hash * 31 + char.charCodeAt(0)) >>> 0;
  return hash % 360;
}

export function botColor(botId: string): string {
  return identityColor(botHue(botId));
}

export function BotAvatar({
  botId,
  size = 24,
  className,
  color,
}: {
  botId: string;
  size?: number;
  className?: string;
  /** Override the derived colour (a template that has no identity yet). */
  color?: string;
}) {
  return (
    <span
      className={cn("inline-grid shrink-0 place-items-center rounded-md text-white", className)}
      style={{ width: size, height: size, background: color ?? botColor(botId) }}
      aria-hidden
    >
      <BotFace size={Math.round(size * 0.72)} />
    </span>
  );
}
