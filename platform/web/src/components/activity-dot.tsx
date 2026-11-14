import type { SessionActivity } from "@lightspeed-ai/sdk";
import { StatusDot, type BotTone } from "@/components/bot/status";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

/** The dot for what a session is doing: working, waiting, idle or closed. */
export function activityTone(activity: SessionActivity | undefined, closed = false): BotTone {
  if (closed) return "closed";
  if (activity === "waiting") return "waiting";
  if (activity === "working") return "live";
  return "idle";
}

/** A word for an active session; idle needs none. */
export function activityLabel(activity: SessionActivity | undefined): string | null {
  if (activity === "waiting") return "Waiting for approval";
  if (activity === "working") return "Working";
  return null;
}

/** What an active session is doing, for the dot's tooltip. */
function activityHint(activity: SessionActivity | undefined): string | null {
  if (activity === "waiting") return "Waiting for approval: a run is paused until someone decides on a tool call";
  if (activity === "working") return "Working: a run is in progress";
  return null;
}

/** Many sessions as one: waiting when any waits, else working when any works. */
export function foldActivity(activities: Iterable<SessionActivity | undefined>): SessionActivity {
  let folded: SessionActivity = "idle";
  for (const activity of activities) {
    if (activity === "waiting") return "waiting";
    if (activity === "working") folded = "working";
  }
  return folded;
}

/**
 * A session's dot. Lists pass `quiet` so idle rows stay unmarked; tabs show
 * every state.
 */
export function ActivityDot({
  activity,
  closed = false,
  quiet = false,
  className,
}: {
  activity: SessionActivity | undefined;
  closed?: boolean;
  quiet?: boolean;
  className?: string;
}) {
  const tone = activityTone(activity, closed);
  if (quiet && (tone === "idle" || tone === "closed")) return null;
  const label = activityLabel(activity);
  const dot = <StatusDot tone={tone} className={className} />;
  if (!label) return <span className="inline-flex">{dot}</span>;
  return (
    <Tooltip>
      <TooltipTrigger render={<span className="inline-flex" aria-label={label} />}>{dot}</TooltipTrigger>
      <TooltipContent>{activityHint(activity)}</TooltipContent>
    </Tooltip>
  );
}
