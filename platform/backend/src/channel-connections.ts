import { slugify } from "@lightspeed-ai/platform-shared";

export interface TelegramBotIdentity {
  id: number;
  firstName: string;
  username: string;
}

export class TelegramConnectionError extends Error {
  constructor(
    message: string,
    readonly status: 400 | 502,
  ) {
    super(message);
  }
}

interface TelegramGetMeResponse {
  ok?: boolean;
  description?: string;
  result?: {
    id?: number;
    is_bot?: boolean;
    first_name?: string;
    username?: string;
  };
}

/**
 * Validate a pasted bot token without ever returning or logging it. Telegram
 * is the authority for the provider identity; users should not have to copy
 * the username separately from BotFather.
 */
export async function readTelegramBotIdentity(
  token: string,
  options: { fetch?: typeof fetch; timeoutMs?: number } = {},
): Promise<TelegramBotIdentity> {
  const request = options.fetch ?? fetch;
  const timeoutMs = options.timeoutMs ?? 5_000;
  let response: Response;
  try {
    response = await request(`https://api.telegram.org/bot${token}/getMe`, {
      signal: AbortSignal.timeout(timeoutMs),
    });
  } catch {
    throw new TelegramConnectionError("Telegram could not be reached; try again", 502);
  }

  let body: TelegramGetMeResponse;
  try {
    body = (await response.json()) as TelegramGetMeResponse;
  } catch {
    throw new TelegramConnectionError("Telegram returned an invalid response", 502);
  }
  if (!response.ok || body.ok !== true) {
    throw new TelegramConnectionError(
      body.description?.trim() || "Telegram rejected this bot token",
      400,
    );
  }
  const bot = body.result;
  const botId = bot?.id;
  if (
    !bot
    || bot.is_bot !== true
    || typeof botId !== "number"
    || !Number.isSafeInteger(botId)
    || typeof bot.first_name !== "string"
    || !bot.first_name.trim()
    || typeof bot.username !== "string"
    || !bot.username.trim()
  ) {
    throw new TelegramConnectionError("Telegram returned an incomplete bot identity", 502);
  }
  return {
    id: botId,
    firstName: bot.first_name.trim(),
    username: bot.username.trim(),
  };
}

export function telegramChannelAccountId(username: string): string {
  return `telegram-${slugify(username)}`;
}

export function whatsAppChannelAccountId(phoneNumber: string): string {
  return `whatsapp-${slugify(phoneNumber)}`;
}
