import type { BotInput, BotView } from "@lightspeed-ai/sdk";

/// A bot id is derived from its name until edited, then it is the person's.
export function botIdFrom(displayName: string): string {
  return displayName
    .toLowerCase()
    .normalize("NFKD")
    .replace(/[\u0300-\u036f]/g, "")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 64);
}

/// "Reviewer · reviewer" reads as a stutter: the id is worth showing next
/// to the name only when it says something the name does not.
export function idIsRedundant(displayName: string | null | undefined, botId: string): boolean {
  return displayName != null && botIdFrom(displayName) === botId;
}

/// The core replaces the bot document whole (PUT with an expected
/// revision), so every edit starts from the record as it stands.
export function botInputOf(bot: BotView): BotInput {
  return {
    botId: bot.botId,
    profileId: bot.profileId,
    displayName: bot.displayName ?? null,
    description: bot.description ?? null,
    brief: bot.brief ?? null,
    runsPerDay: bot.runsPerDay ?? null,
    breaker: bot.breaker ?? null,
    routedSessionCloseAfterMs: bot.routedSessionCloseAfterMs ?? null,
    selfConfig: bot.selfConfig ?? false,
    emit: bot.emit ?? false,
    enabled: bot.enabled ?? true,
  };
}
