import { InboxIcon } from "@/icons";
import type { MetaItem, MetaTone } from "@/components/meta-toolbar";
import type { BotTone } from "./status";

/// A bot's meta items: one list for its list row and its page header.
///
///   status    what it is doing, in words with a dot (Working, Paused, …);
///             not in a list row, whose dot and second line show it
///   pending   how many events wait for it; opens its activity

export const BOT_META = { status: "status", pending: "pending" } as const;

const TONE: Record<BotTone, { tone: MetaTone; pulse?: boolean }> = {
  live: { tone: "success", pulse: true },
  waiting: { tone: "warning" },
  idle: { tone: "muted" },
  paused: { tone: "muted" },
  attention: { tone: "danger" },
  closed: { tone: "muted" },
};

export function botMetaItems({ status, error, pending, closed, activityHref }: {
  status: { label: string; tone: BotTone };
  /// Why it needs attention, for the status's tooltip.
  error?: string | null;
  /// Events waiting to be handled.
  pending: number;
  closed: boolean;
  /// Its activity, where the waiting events are.
  activityHref?: string;
}): MetaItem[] {
  const items: MetaItem[] = [
    { key: BOT_META.status, label: status.label, value: status.label, ...TONE[status.tone], tooltip: error ?? null, priority: 10, places: ["header", "strip"] },
  ];
  if (pending > 0 && !closed) {
    items.push({
      key: BOT_META.pending,
      label: `${pending} waiting`,
      icon: InboxIcon,
      value: pending,
      text: `${pending} waiting`,
      tone: "info",
      tooltip: `${pending} event${pending === 1 ? "" : "s"} waiting to be handled`,
      href: activityHref,
      priority: 8,
    });
  }
  return items;
}
