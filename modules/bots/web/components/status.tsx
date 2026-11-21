import type { ReactNode } from "react";
import { Badge } from "@lightspeed-ai/platform-web/modules/ui/badge";
import type { SessionActivity, BotControllerSnapshot, BotControllerStatus, BotEventOutcome, BotView } from "@lightspeed-ai/sdk";
import type { ActivityTone } from "@lightspeed-ai/platform-web/modules/ui/status";

/** An event's outcome as a person reads it. */
export function outcomeLabel(outcome: BotEventOutcome): string {
  return outcome === "runFailed" ? "run failed" : outcome;
}

/**
 * One status word for the header and the roster, in the person's language:
 * the record's lifecycle first (closed, paused), then what the controller is
 * doing.
 */
export function botStatus(
  bot: Pick<BotView, "enabled" | "closedAtMs">,
  controller: BotControllerSnapshot | undefined,
  stateError?: string,
  activity: SessionActivity = "idle",
): { label: string; tone: ActivityTone } {
  if (bot.closedAtMs != null) return { label: "Closed", tone: "closed" };
  if (bot.enabled === false) return { label: "Paused", tone: "paused" };
  if (stateError) return { label: "Needs attention", tone: "attention" };
  // What its sessions are doing wins over the controller's own state: a run
  // someone started by chatting is work too.
  if (activity === "waiting") return { label: "Waiting for approval", tone: "waiting" };
  if (!controller) return { label: activity === "working" ? "Working" : "Starting", tone: activity === "working" ? "live" : "idle" };
  switch (controller.controllerStatus) {
    case "initializing":
      return { label: "Starting", tone: "idle" };
    case "deliveringEvent": {
      const count = (controller.activeDeliveries ?? []).reduce(
        (sum, delivery) => sum + delivery.seqs.length,
        0,
      );
      return { label: count > 1 ? `Working on ${count} events` : "Working", tone: "live" };
    }
    case "budgetExhausted":
      return { label: `Out of budget until ${utcMidnightLabel()}`, tone: "attention" };
    case "degraded":
      return { label: "Needs attention", tone: "attention" };
    case "closing":
      return { label: "Closing", tone: "closed" };
    case "closed":
      return { label: "Closed", tone: "closed" };
    default:
      return activity === "working" ? { label: "Working", tone: "live" } : { label: "Idle", tone: "idle" };
  }
}

function utcMidnightLabel(): string {
  const next = new Date();
  next.setUTCHours(24, 0, 0, 0);
  return next.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

export function BotStatusBadge({ status }: { status?: BotControllerStatus }) {
  if (!status) return <Badge variant="outline">starting</Badge>;
  if (status === "degraded") return <Badge variant="destructive">needs attention</Badge>;
  if (status === "budgetExhausted") return <Badge variant="destructive">out of budget</Badge>;
  if (status === "idle") return <Badge variant="secondary">idle</Badge>;
  if (status === "deliveringEvent") return <Badge variant="secondary">working</Badge>;
  return <Badge variant="outline">{status}</Badge>;
}

/** A titled block of the bot workspace. */
export function DetailSection({
  title,
  description,
  actions,
  children,
  id,
}: {
  title: string;
  description?: string;
  actions?: ReactNode;
  children: ReactNode;
  id?: string;
}) {
  return (
    <section id={id} className="grid min-w-0 scroll-mt-4 content-start gap-3">
      <div className="flex min-w-0 items-start justify-between gap-3">
        <div className="grid min-w-0 flex-1 gap-0.5">
          <h2 className="text-sm font-semibold">{title}</h2>
          {description && <p className="text-xs text-muted-foreground">{description}</p>}
        </div>
        {actions}
      </div>
      {children}
    </section>
  );
}

export function KeyValue({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className="grid grid-cols-[7rem_minmax(0,1fr)] gap-2 text-xs">
      <span className="text-muted-foreground">{label}</span>
      <span className="min-w-0 truncate">{value}</span>
    </div>
  );
}
