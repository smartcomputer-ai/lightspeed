import type { ChannelPresenceActivities } from "../../contracts/presence.js";
import { createTypingActivities } from "../typing.js";

export interface TelegramPresenceApi {
  sendChatAction(
    chatId: string,
    action: "typing",
    options: { message_thread_id?: number },
  ): Promise<unknown>;
}

export function createTelegramPresenceActivities(config: {
  accountId: string;
  api: TelegramPresenceApi;
}): ChannelPresenceActivities {
  return createTypingActivities({
    provider: "telegram",
    accountId: config.accountId,
    intervalMs: 4_000,
    pulse: async ({ route }) => {
      await config.api.sendChatAction(route.chatId, "typing", {
        ...(route.threadId === undefined
          ? {}
          : { message_thread_id: positiveInteger(route.threadId) }),
      });
    },
    clear: async () => undefined,
  });
}

function positiveInteger(value: string): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new TypeError(`invalid Telegram thread id: ${value}`);
  }
  return parsed;
}
