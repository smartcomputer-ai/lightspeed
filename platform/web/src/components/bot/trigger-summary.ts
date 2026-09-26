import type { BotTriggerView, ChannelAccountView } from "@/api";
import { cronBuilderFromExpression, cronFromBuilder, type CronBuilderState } from "./cron-builder";

const WEEKDAY_NAMES = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

function timeLabel(time: string): string {
  const match = /^(\d{2}):(\d{2})$/.exec(time);
  return match ? `${match[1]}:${match[2]}` : time;
}

/** A cron expression in words when the builder can express it, else the expression itself. */
export function describeCron(cron: string, timezone?: string | null): string {
  const trimmed = cron.trim();
  if (!trimmed) return "no schedule";
  const state: CronBuilderState = cronBuilderFromExpression(trimmed);
  const exact = cronFromBuilder(state) === trimmed;
  const zone = timezone && timezone !== "UTC" ? ` ${timezone}` : timezone === "UTC" ? " UTC" : "";
  if (!exact) return `${trimmed}${zone}`;
  switch (state.frequency) {
    case "minutes":
      return `Every ${state.interval} minutes`;
    case "hourly":
      return state.minute === 0 ? "Every hour" : `Every hour at :${String(state.minute).padStart(2, "0")}`;
    case "daily":
      return `Every day at ${timeLabel(state.time)}${zone}`;
    case "weekdays":
      return `Weekdays at ${timeLabel(state.time)}${zone}`;
    case "weekly":
      return `${WEEKDAY_NAMES[state.weekday] ?? "Weekly"}s at ${timeLabel(state.time)}${zone}`;
    case "monthly":
      return `Monthly on day ${state.monthday} at ${timeLabel(state.time)}${zone}`;
  }
}

function hostOf(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/**
 * One line saying what wakes the bot, in the person's words. Chat triggers
 * carry only the account id; pass the universe's channel accounts to name
 * the account (there is no server-side join on the trigger).
 */
export function triggerSummary(
  trigger: BotTriggerView,
  accounts?: Pick<ChannelAccountView, "accountId" | "provider" | "displayName">[],
): string {
  switch (trigger.kind) {
    case "schedule": {
      if (trigger.atMs != null) return `Once, at ${new Date(trigger.atMs).toLocaleString()}`;
      return describeCron(trigger.cron ?? "", trigger.timezone ?? null);
    }
    case "webhook": {
      const source = trigger.preset === "github" ? "GitHub webhook" : "Webhook";
      const verified = trigger.verification?.scheme === "hmac-sha256" ? "signed" : "URL token";
      return `${source} · ${verified}`;
    }
    case "poll": {
      const every = `every ${Math.max(1, Math.round(trigger.intervalMs / 60_000))} min`;
      return trigger.source.kind === "http"
        ? `Checks ${hostOf(trigger.source.url)} ${every}`
        : `Runs ${trigger.source.argv[0] ?? "a command"} ${every}`;
    }
    case "chat": {
      const account = accounts?.find((entry) => entry.accountId === trigger.accountId);
      const name = account ? `${account.provider} · ${account.displayName}` : "a messaging account";
      const scope =
        trigger.matchScope === "direct" ? "direct messages" : trigger.matchScope === "group" ? "groups" : "all chats";
      return `${name} · ${scope}${(trigger.pairing ?? "code") === "code" ? " · pairing required" : ""}`;
    }
    case "bot": {
      return trigger.from == null
        ? "Messages from any bot in this universe"
        : trigger.from.length === 0
          ? "Messages from no bot yet"
          : `Messages from ${trigger.from.join(", ")}`;
    }
  }
}

export interface DeliveryShape {
  routePolicy: "bot" | "perKey" | "perEvent";
  routeKey: string;
  filter: string;
  whenBusy: "queue" | "steer" | "append";
  /** Seconds, as typed; empty means no coalescing. */
  debounceSeconds: string;
  maxWaitSeconds: string;
  closeMode: "inherit" | "forever" | "hours";
  closeHours: string;
}

/**
 * The delivery disclosure, closed: what routing, batching, busy handling,
 * and idle close do, as one sentence. Reads the same for a form and a saved
 * trigger, so a person learns the vocabulary before opening the fields.
 */
export function deliverySentence(shape: DeliveryShape, chat = false): string {
  const parts: string[] = [];
  if (shape.routePolicy === "bot") parts.push("to Main");
  else if (shape.routePolicy === "perKey") {
    parts.push(
      chat
        ? "one thread per conversation"
        : shape.routeKey.trim()
          ? `one thread per ${shape.routeKey.trim()}`
          : "one thread per key",
    );
  } else parts.push(chat ? "one thread per message" : "one thread per event");
  if (shape.filter.trim()) parts.push("filtered");
  const debounce = Number(shape.debounceSeconds);
  if (shape.debounceSeconds.trim() !== "" && debounce > 0) {
    const wait = Number(shape.maxWaitSeconds);
    const bound = shape.maxWaitSeconds.trim() !== "" && wait > debounce ? wait : debounce;
    parts.push(`batches for up to ${bound % 1 === 0 ? bound : bound.toFixed(1)}s`);
  } else parts.push("no batching");
  parts.push(
    shape.whenBusy === "steer"
      ? "steers a busy run"
      : shape.whenBusy === "append"
        ? "context only when busy"
        : "queues when busy",
  );
  if (shape.routePolicy !== "bot") {
    if (shape.closeMode === "forever") parts.push("threads kept");
    else if (shape.closeMode === "hours" && shape.closeHours.trim()) parts.push(`threads close after ${shape.closeHours.trim()}h idle`);
  }
  return parts.join(" · ");
}

export function deliveryShapeOf(trigger: BotTriggerView): DeliveryShape {
  return {
    routePolicy: trigger.route?.policy ?? (trigger.kind === "chat" ? "perKey" : "bot"),
    routeKey: trigger.route?.policy === "perKey" ? (trigger.route.key ?? "") : "",
    filter: trigger.filter ?? "",
    whenBusy: trigger.deliver?.whenBusy ?? "queue",
    debounceSeconds: trigger.coalesce ? String(trigger.coalesce.debounceMs / 1000) : "",
    maxWaitSeconds: trigger.coalesce ? String(trigger.coalesce.maxWaitMs / 1000) : "",
    closeMode: trigger.sessionCloseAfterMs == null ? "inherit" : trigger.sessionCloseAfterMs === 0 ? "forever" : "hours",
    closeHours: trigger.sessionCloseAfterMs ? String(Math.round(trigger.sessionCloseAfterMs / 3_600_000)) : "",
  };
}
