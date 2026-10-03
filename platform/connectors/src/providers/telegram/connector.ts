import { Bot, GrammyError, type Context } from "grammy";
import type { LightspeedClient } from "@lightspeed-ai/sdk";
import type { TokenSource } from "../../core/leases.js";
import type {
  ConnectorActivities,
  InboundGate,
  IngressHealth,
  ProviderConnector,
} from "../connector.js";
import { ChannelDeliveryError } from "../delivery.js";
import { createTelegramDeliveryActivities } from "./delivery.js";
import { normalizeTelegramInbound, type TelegramInboundMessage } from "./ingress.js";
import { createTelegramMediaActivities } from "./media.js";
import { createTelegramPresenceActivities } from "./presence.js";

type TelegramMessage = NonNullable<Context["message"]>;

export interface TelegramConnectorConfig {
  universeId: string;
  accountId: string;
  /** The leased bot token (`auth/grants/lease`); re-leased when Telegram rejects it. */
  token: TokenSource;
  gate: InboundGate;
  health: IngressHealth;
  /** Universe-scoped core client (media uploads). */
  core: Pick<LightspeedClient, "call">;
  log?: Pick<Console, "log" | "warn" | "error">;
  /** Pause before re-leasing after Telegram rejected the token or another consumer polls it. */
  retryDelayMs?: number;
  fetch?: typeof fetch;
}

/**
 * One Telegram account: grammy long polling for ingress and the account's
 * activities over the same bot. Telegram allows one `getUpdates` consumer per
 * token, so one account is served by exactly one host.
 */
export function createTelegramConnector(config: TelegramConnectorConfig): ProviderConnector {
  const log = config.log ?? console;
  const retryDelayMs = config.retryDelayMs ?? 5_000;
  let bot: Bot | undefined;
  let stopped = false;
  let wake: (() => void) | undefined;

  const current = (): Bot => {
    if (bot === undefined) {
      throw new ChannelDeliveryError("Telegram bot is not connected", true);
    }
    return bot;
  };

  const activities: ConnectorActivities = {
    ...createTelegramDeliveryActivities({
      accountId: config.accountId,
      api: {
        sendMessage: (chatId, text, options) => current().api.sendMessage(chatId, text, options),
        editMessageText: (chatId, messageId, text, options) =>
          current().api.editMessageText(chatId, messageId, text, options),
        setMessageReaction: (chatId, messageId, reactions) =>
          current().api.setMessageReaction(chatId, messageId, reactions as never),
      },
    }),
    ...createTelegramMediaActivities({
      universeId: config.universeId,
      accountId: config.accountId,
      botToken: config.token,
      core: config.core,
      api: { getFile: (fileId) => current().api.getFile(fileId) },
      ...(config.fetch === undefined ? {} : { fetch: config.fetch }),
    }),
    ...createTelegramPresenceActivities({
      accountId: config.accountId,
      api: {
        sendChatAction: (chatId, action, options) =>
          current().api.sendChatAction(chatId, action, options),
      },
    }),
  };

  async function handleMessage(next: Bot, message: TelegramMessage): Promise<void> {
    if (message.from === undefined) {
      return;
    }
    const inbound = normalizeTelegramInbound(
      {
        botId: next.botInfo.id,
        ...(next.botInfo.username === undefined ? {} : { botUsername: next.botInfo.username }),
      },
      toInboundMessage(message),
    );
    if (inbound === null) {
      return;
    }
    // The gate resolves once the core holds the message; grammy advances its
    // update offset only after this handler returns.
    const verdict = await config.gate.admit(inbound);
    if (verdict.reply !== null) {
      await next.api.sendMessage(message.chat.id, verdict.reply, {
        ...(message.message_thread_id === undefined
          ? {}
          : { message_thread_id: message.message_thread_id }),
        reply_parameters: { message_id: message.message_id },
      });
    }
  }

  async function run(): Promise<void> {
    while (!stopped) {
      const token = await config.token.get();
      const next = new Bot(token);
      try {
        await next.init();
        if (stopped) return;
        bot = next;
        next.catch((error) => {
          log.error(`connectors: Telegram ${config.accountId} ingress handler failed`, error.error);
        });
        next.on("message", (ctx) => handleMessage(next, ctx.message));
        await next.start({
          onStart: () => {
            config.health.markIngressConnected();
            log.log(`connectors: Telegram ${config.accountId} (@${next.botInfo.username}) ingress ready`);
          },
        });
        return;
      } catch (error) {
        if (bot === next) bot = undefined;
        if (stopped) return;
        if (isTokenRejected(error)) {
          config.token.invalidate();
          config.health.markReconnectScheduled("Telegram rejected the bot token; re-leasing");
          log.warn(`connectors: Telegram ${config.accountId} token rejected; re-leasing`);
        } else if (isPollingConflict(error)) {
          config.health.markReconnectScheduled("another consumer is polling this bot token");
          log.warn(`connectors: Telegram ${config.accountId} getUpdates conflict; retrying`);
        } else {
          throw error;
        }
        await pause(retryDelayMs);
      }
    }
  }

  async function stop(): Promise<void> {
    stopped = true;
    wake?.();
    const running = bot;
    bot = undefined;
    if (running?.isRunning()) {
      await running.stop();
    }
  }

  function pause(milliseconds: number): Promise<void> {
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        wake = undefined;
        resolve();
      }, milliseconds);
      wake = () => {
        clearTimeout(timer);
        wake = undefined;
        resolve();
      };
    });
  }

  return { activities, run, stop };
}

function isTokenRejected(error: unknown): boolean {
  return error instanceof GrammyError && error.error_code === 401;
}

function isPollingConflict(error: unknown): boolean {
  return error instanceof GrammyError && error.error_code === 409;
}

function toInboundMessage(message: TelegramMessage): TelegramInboundMessage {
  const from = message.from!;
  return {
    messageId: message.message_id,
    chatId: message.chat.id,
    chatType: message.chat.type,
    ...(message.message_thread_id === undefined ? {} : { threadId: message.message_thread_id }),
    senderId: from.id,
    ...(from.username === undefined ? {} : { senderUsername: from.username }),
    ...(from.first_name === undefined ? {} : { senderFirstName: from.first_name }),
    ...(from.last_name === undefined ? {} : { senderLastName: from.last_name }),
    timestampMs: message.date * 1_000,
    ...(message.text === undefined ? {} : { text: message.text }),
    ...(message.caption === undefined ? {} : { caption: message.caption }),
    ...(message.entities === undefined
      ? {}
      : { entities: message.entities.map(({ type, offset, length }) => ({ type, offset, length })) }),
    ...(message.caption_entities === undefined
      ? {}
      : {
          captionEntities: message.caption_entities.map(({ type, offset, length }) => ({
            type,
            offset,
            length,
          })),
        }),
    ...(message.reply_to_message?.from?.id === undefined
      ? {}
      : { replyToSenderId: message.reply_to_message.from.id }),
    ...(message.photo === undefined
      ? {}
      : {
          photos: message.photo.map((photo) => ({
            fileId: photo.file_id,
            width: photo.width,
            height: photo.height,
            ...(photo.file_size === undefined ? {} : { fileSize: photo.file_size }),
          })),
        }),
    ...(message.document === undefined
      ? {}
      : {
          document: {
            fileId: message.document.file_id,
            ...(message.document.file_size === undefined
              ? {}
              : { fileSize: message.document.file_size }),
            ...(message.document.file_name === undefined
              ? {}
              : { fileName: message.document.file_name }),
            ...(message.document.mime_type === undefined
              ? {}
              : { mimeType: message.document.mime_type }),
          },
        }),
    ...(message.voice === undefined
      ? {}
      : {
          voice: {
            fileId: message.voice.file_id,
            ...(message.voice.file_size === undefined ? {} : { fileSize: message.voice.file_size }),
            ...(message.voice.mime_type === undefined ? {} : { mimeType: message.voice.mime_type }),
          },
        }),
    ...(message.audio === undefined
      ? {}
      : {
          audio: {
            fileId: message.audio.file_id,
            ...(message.audio.file_size === undefined ? {} : { fileSize: message.audio.file_size }),
            ...(message.audio.file_name === undefined ? {} : { fileName: message.audio.file_name }),
            ...(message.audio.mime_type === undefined ? {} : { mimeType: message.audio.mime_type }),
          },
        }),
  };
}
