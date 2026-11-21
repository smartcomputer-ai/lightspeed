import { cn } from "@/lib/utils";

export type ActivityTone = "live" | "waiting" | "idle" | "paused" | "attention" | "closed";
export type StatusTone = ActivityTone | "awaiting" | "ready";

/// Each tone's color, as the text color the dot (and its ring) is drawn in.
const STATUS_DOT_COLOR: Record<StatusTone, string> = {
  live: "text-emerald-500",
  awaiting: "text-emerald-500",
  ready: "text-emerald-500",
  waiting: "text-amber-500",
  idle: "text-muted-foreground/50",
  paused: "text-amber-500",
  attention: "text-destructive",
  closed: "text-muted-foreground/30",
};

/// The ring elements every status dot holds (here and in the meta toolbar):
/// the CSS shows and moves as many as its data-tone sends.
export function StatusDotRings() {
  return (
    <>
      <span className="status-dot-ring" />
      <span className="status-dot-ring" />
      <span className="status-dot-ring" />
    </>
  );
}

/// A status dot in its tone's color. How it is drawn and moves (the live
/// pulse, the rings, the hollow paused and awaiting dots) is all CSS: .status-dot in
/// index.css, by data-tone. It always holds three ring elements; the CSS
/// shows and moves as many as the tone sends (none, one, or three for live).
export function StatusDot({ tone, className }: { tone: StatusTone; className?: string }) {
  return (
    <span
      data-tone={tone}
      className={cn("status-dot inline-block size-2 shrink-0 rounded-full", STATUS_DOT_COLOR[tone], className)}
      aria-hidden
    >
      <StatusDotRings />
    </span>
  );
}
