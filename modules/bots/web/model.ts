import type { BotView } from "@lightspeed-ai/sdk";

/// What lets an outsider reach a trigger. Never part of a trigger as it is
/// read or listed: members who may manage the bot read it from
/// `GET .../triggers/:triggerId/secrets`, and creating or replacing a trigger
/// answers with it beside the trigger.
export interface BotTriggerSecrets {
  /// Webhook triggers: the ingest path including its URL token.
  ingestPath?: string | null;
}

export function botLabel(bot: Pick<BotView, "botId" | "displayName">): string {
  return bot.displayName ?? bot.botId;
}
