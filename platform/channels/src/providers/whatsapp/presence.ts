import type { ChannelPresenceActivities } from "../../contracts/presence.js";
import { createTypingActivities } from "../typing.js";

export interface WhatsAppPresenceApi {
  sendPresenceUpdate(state: "composing" | "paused", jid: string): Promise<void>;
}

export function createWhatsAppPresenceActivities(config: {
  accountId: string;
  api: WhatsAppPresenceApi;
}): ChannelPresenceActivities {
  return createTypingActivities({
    provider: "whatsapp",
    accountId: config.accountId,
    intervalMs: 8_000,
    pulse: ({ route }) => config.api.sendPresenceUpdate("composing", route.chatId),
    clear: ({ route }) => config.api.sendPresenceUpdate("paused", route.chatId),
  });
}
