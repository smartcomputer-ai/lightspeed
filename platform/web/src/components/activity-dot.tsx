import type { SessionActivity } from "@lightspeed-ai/sdk";
import type { SessionSummary } from "@/api";
import { StatusDot, type ActivityTone, type StatusTone } from "@/components/status-dot";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

/** The dot for what a session is doing: working, waiting, idle or closed. */
export function activityTone(activity: SessionActivity | undefined, closed = false): ActivityTone {
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

/** What a session's dot reads from its summary. */
export type SessionDotInput = Pick<SessionSummary, "activity" | "lifecycleStatus">
  & Partial<Pick<SessionSummary, "executionFailure" | "activityPhase" | "lastRun">>;

/**
 * A session's state for its dot in a list, most urgent first: closed, a
 * stopped workflow, a run waiting for a person, a run parked on work it
 * started, a run in progress, then how the last run ended. `label` names
 * the state and `hint` explains it; an idle session has neither.
 */
export function sessionDotState(session: SessionDotInput): { tone: StatusTone; label: string | null; hint: string | null } {
  if (session.lifecycleStatus === "closed") return { tone: "closed", label: null, hint: null };
  if (session.executionFailure) {
    return { tone: "attention", label: "Stopped", hint: "Stopped: the session's workflow is no longer running" };
  }
  if (session.activity === "waiting") {
    return { tone: "waiting", label: activityLabel("waiting"), hint: activityHint("waiting") };
  }
  if (session.activityPhase === "awaiting") {
    return { tone: "awaiting", label: "Awaiting", hint: "Awaiting: a run is parked until work it started finishes" };
  }
  if (session.activity === "working") {
    return { tone: "live", label: activityLabel("working"), hint: activityHint("working") };
  }
  if (session.lastRun?.status === "failed") {
    return { tone: "attention", label: "Last run failed", hint: "Last run failed: open the session to see why" };
  }
  return { tone: "idle", label: null, hint: null };
}

/** A dot, with its state as the accessible name and tooltip when it has one. */
function LabelledDot({ tone, label, hint, className }: {
  tone: StatusTone;
  label: string | null;
  hint: string | null;
  className?: string;
}) {
  const dot = <StatusDot tone={tone} className={className} />;
  if (!label) return <span className="inline-flex">{dot}</span>;
  return (
    <Tooltip>
      <TooltipTrigger render={<span className="inline-flex" aria-label={label} />}>{dot}</TooltipTrigger>
      <TooltipContent>{hint}</TooltipContent>
    </Tooltip>
  );
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
  return <LabelledDot tone={tone} label={activityLabel(activity)} hint={activityHint(activity)} className={className} />;
}

/**
 * A session's dot where its summary is at hand (the sessions list): besides
 * what a run is doing, it marks a stopped workflow, a parked run and a
 * failed last run.
 */
export function SessionDot({ session, className }: { session: SessionDotInput; className?: string }) {
  return <LabelledDot {...sessionDotState(session)} className={className} />;
}
