import type { ReactNode } from "react";
import { Badge } from "@/components/ui/badge";
import type { SessionActivity } from "@lightspeed-ai/agent-client";
import type { BotControllerSnapshot, BotControllerStatus, BotView } from "@/api";
import { cn } from "@/lib/utils";

export type BotTone = "live" | "waiting" | "idle" | "paused" | "attention" | "closed";

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
): { label: string; tone: BotTone } {
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
    case "delivering_event": {
      const count = (controller.activeDeliveries ?? []).reduce(
        (sum, delivery) => sum + delivery.seqs.length,
        0,
      );
      return { label: count > 1 ? `Working on ${count} events` : "Working", tone: "live" };
    }
    case "budget_exhausted":
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

export function StatusDot({ tone, className }: { tone: BotTone; className?: string }) {
  return (
    <span
      className={cn(
        "inline-block size-2 shrink-0 rounded-full",
        tone === "live" && "animate-pulse bg-emerald-500 motion-reduce:animate-none",
        tone === "waiting" && "bg-amber-500",
        tone === "idle" && "bg-muted-foreground/50",
        tone === "paused" && "border border-amber-500 bg-transparent",
        tone === "attention" && "bg-destructive",
        tone === "closed" && "bg-muted-foreground/30",
        className,
      )}
      aria-hidden
    />
  );
}

export function BotStatusBadge({ status }: { status?: BotControllerStatus }) {
  if (!status) return <Badge variant="outline">starting</Badge>;
  if (status === "degraded") return <Badge variant="destructive">needs attention</Badge>;
  if (status === "budget_exhausted") return <Badge variant="destructive">out of budget</Badge>;
  if (status === "idle") return <Badge variant="secondary">idle</Badge>;
  if (status === "delivering_event") return <Badge variant="secondary">working</Badge>;
  return <Badge variant="outline">{status.replaceAll("_", " ")}</Badge>;
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

export function relativeTime(iso: string | number): string {
  const ms = typeof iso === "number" ? iso : new Date(iso).getTime();
  if (!Number.isFinite(ms)) return "";
  const delta = Date.now() - ms;
  if (delta < 60_000) return "now";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)}h`;
  return `${Math.floor(delta / 86_400_000)}d`;
}
