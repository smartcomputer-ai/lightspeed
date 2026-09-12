import type { BotView, Environment, ProfileEnvironment } from "@/api";
import { describeIdlePolicy } from "@/components/environment/power-controls";

const FEATURE_LABELS: Record<string, string> = {
  web: "Web",
  vfs: "Files",
  environments: "Environment tools",
  mcp: "MCP servers",
  subagents: "Sub-agents",
  timers: "Timers",
};

/** The model and capability grants of a session config, in words. */
export function capabilitySummary(config: Record<string, unknown> | undefined): string[] {
  const features = (config?.features ?? {}) as Record<string, unknown>;
  const labels = Object.keys(features).map((key) => {
    const value = features[key] as Record<string, unknown> | undefined;
    if (key === "mcp" && Array.isArray(value?.servers)) return `MCP servers (${value.servers.length})`;
    if (key === "subagents" && Array.isArray(value?.agents)) return `Sub-agents (${value.agents.length})`;
    return FEATURE_LABELS[key] ?? key;
  });
  const model = (config?.model as { model?: string } | undefined)?.model;
  return [...(model ? [model] : []), ...labels];
}

export function environmentSummary(
  environment: ProfileEnvironment | null | undefined,
  environments?: Environment[],
): string {
  if (!environment) return "No environment";
  if (environment.type === "existing") {
    const current = environments?.find((entry) => entry.environmentId === environment.environmentId);
    const name = current?.displayName ?? environment.environmentId;
    const policy = current?.idlePolicy ? ` · ${describeIdlePolicy(current.idlePolicy)}` : "";
    return `${name}${current ? ` · ${current.status}` : ""}${policy}`;
  }
  return "Inherits the session's environment";
}

export function briefSummary(brief: string | null | undefined): string {
  const text = brief?.trim() ?? "";
  if (!text) return "No brief yet";
  const line = text.split(/\n/)[0] ?? "";
  return line.length > 120 ? `${line.slice(0, 120)}…` : line;
}

export function guardrailsSummary(
  bot: Pick<BotView, "runsPerDay" | "breaker" | "routedSessionCloseAfterMs" | "selfConfig">,
): string {
  return [
    bot.runsPerDay == null ? "no daily limit" : `${bot.runsPerDay} runs a day`,
    bot.breaker ? `flood ${bot.breaker.fires}/${Math.round(bot.breaker.windowMs / 60_000)} min` : null,
    bot.routedSessionCloseAfterMs ? `threads close after ${Math.round(bot.routedSessionCloseAfterMs / 86_400_000)}d` : "threads kept",
    bot.selfConfig ? "can change own triggers" : null,
  ]
    .filter((part): part is string => part !== null)
    .join(" · ");
}

/** Both directions of bot-to-bot messaging in one line. */
export function otherBotsSummary(emit: boolean, inbox: "off" | "any" | string[]): string {
  const receive =
    inbox === "off" ? "receives from nobody" : inbox === "any" ? "receives from any bot" : `receives from ${inbox.join(", ")}`;
  return `${emit ? "can send" : "cannot send"} · ${receive}`;
}
