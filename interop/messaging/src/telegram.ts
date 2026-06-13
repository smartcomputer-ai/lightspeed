import { Bot, type Context } from "grammy";
import type { Message } from "grammy/types";
import type { TelegramBridgeConfig } from "./config.js";
import { stableHash } from "./ids.js";
import { shouldQuoteChunk, type ReplyToMode } from "./policy.js";
import type { OutboundMessageView } from "@lightspeed/agent-client";
import { DeliveryError, type ChannelDeliverer, type DeliveryResult } from "./outbox.js";
import type {
  ChannelPolicy,
  InboundMedia,
  MessagingBridgeRuntime,
  NormalizedInbound,
} from "./runtime.js";
import { renderTelegramHtml } from "./markdown.js";
import { documentByteLimit, documentMime } from "./media.js";
import type { BindingState } from "./store.js";
import { splitMessageText } from "./text.js";

/// Telegram bot file downloads are capped at 20MB; the gateway caps images
/// at 10MB anyway.
const MAX_TELEGRAM_IMAGE_BYTES = 10 * 1024 * 1024;

export interface RunningBridge {
  stop: () => Promise<void>;
  deliverer: ChannelDeliverer;
}

export async function startTelegramBridge(
  config: TelegramBridgeConfig,
  runtime: MessagingBridgeRuntime,
): Promise<RunningBridge> {
  const bot = new Bot(config.botToken);
  const me = await bot.api.getMe();
  const botUsername = me.username ?? null;
  const allowedChatIds = new Set(config.allowedChatIds.map(String));
  const allowFrom = new Set(config.allowFrom.map(String));

  if (allowedChatIds.size === 0) {
    console.warn("telegram: TELEGRAM_ALLOWED_CHAT_IDS is empty; all chats can trigger the bridge");
  }

  const policy: ChannelPolicy = {
    triggerPrefixes: config.triggerPrefixes,
    mentionNames: config.mentionNames,
    botUsername,
    groupActivation: config.groupActivation,
  };

  bot.catch((error) => {
    console.error("telegram: handler failed", error.error);
  });

  const handleMessage = async (ctx: Context & { message: Message }) => {
    const message = ctx.message;
    const chatId = String(message.chat.id);
    if (allowedChatIds.size > 0 && !allowedChatIds.has(chatId)) {
      return;
    }

    const text = message.text ?? message.caption ?? "";
    const hasPhoto = (message.photo?.length ?? 0) > 0;
    const documentMimeType = message.document
      ? documentMime(message.document.file_name, message.document.mime_type)
      : null;
    const hasDocument = documentMimeType !== null;
    if (!text && !hasPhoto && !hasDocument) {
      return;
    }

    const isDirect = message.chat.type === "private";
    const senderId = message.from ? String(message.from.id) : "unknown";
    const threadId = message.message_thread_id;
    const conversationParts = ["telegram", config.accountId, chatId, threadId ?? "main"];
    const conversationKey = `telegram:${stableHash(conversationParts)}`;
    const messageKey = `telegram:${stableHash([
      config.accountId,
      chatId,
      threadId ?? "main",
      message.message_id,
    ])}`;

    const inbound: NormalizedInbound = {
      provider: "telegram",
      accountId: config.accountId,
      chatId,
      ...(threadId !== undefined ? { threadId: String(threadId) } : {}),
      conversationKey,
      conversationParts,
      messageId: String(message.message_id),
      messageKey,
      senderId,
      senderName: senderDisplayName(message),
      timestampMs: message.date * 1000,
      text:
        text ||
        (hasDocument
          ? `(sent a file: ${message.document?.file_name ?? "document"})`
          : "(sent an image)"),
      isDirect,
      chatLabel: isDirect ? "dm" : (message.chat.title ?? chatId),
      mentionedBot: messageMentionsBot(message, me.id, botUsername),
      isReplyToBot: message.reply_to_message?.from?.id === me.id,
      isFromSelf: message.from?.id === me.id,
      // With no explicit allowFrom, direct chats in the chat allowlist are
      // trusted for control commands; group members are not.
      senderAllowed: allowFrom.size > 0 ? allowFrom.has(senderId) : isDirect,
      ...(hasPhoto
        ? { fetchMedia: () => downloadTelegramPhoto(ctx, config.botToken, message) }
        : hasDocument
          ? {
              fetchMedia: () =>
                downloadTelegramDocument(ctx, config.botToken, message, documentMimeType),
            }
          : {}),
    };

    await runtime.handleInbound(inbound, policy, {
      sendReply: async (replyText) => {
        await sendTelegramReply(ctx, replyText, message.message_id, config.replyToMode, isDirect);
      },
      setTyping: async () => {
        await bot.api.sendChatAction(message.chat.id, "typing", {
          ...(threadId !== undefined ? { message_thread_id: threadId } : {}),
        });
      },
    });
  };

  bot.on("message:text", handleMessage);
  bot.on("message:photo", handleMessage);
  bot.on("message:document", handleMessage);

  const polling = bot.start({ allowed_updates: ["message"] }).catch((error) => {
    console.error("telegram: polling stopped", error);
  });
  console.log(
    `telegram: started @${botUsername ?? me.id} as account ${config.accountId} (group activation: ${config.groupActivation})`,
  );
  if (config.groupActivation !== "mention") {
    console.log(
      "telegram: ambient group modes need BotFather privacy mode disabled to receive all group messages",
    );
  }

  return {
    stop: async () => {
      await bot.stop();
      await polling;
    },
    deliverer: {
      channel: "telegram",
      accountId: config.accountId,
      deliver: (binding, payload) => deliverTelegramPayload(bot, binding, payload),
    },
  };
}

async function deliverTelegramPayload(
  bot: Bot,
  binding: BindingState,
  payload: OutboundMessageView["payload"],
): Promise<DeliveryResult> {
  const chatId = binding.chatId;
  const threadId = binding.threadId !== undefined ? Number(binding.threadId) : undefined;
  try {
    switch (payload.type) {
      case "send": {
        const chunks = splitMessageText(payload.text, 4_000);
        let lastMessageId: number | undefined;
        for (const [index, chunk] of chunks.entries()) {
          const options = {
            ...(threadId !== undefined && Number.isFinite(threadId)
              ? { message_thread_id: threadId }
              : {}),
            ...(index === 0 && payload.replyTo !== undefined && payload.replyTo !== null
              ? { reply_parameters: { message_id: Number(payload.replyTo) } }
              : {}),
          };
          const sent = await sendWithFormatting(
            (text, parseMode) =>
              bot.api.sendMessage(chatId, text, { ...options, ...parseMode }),
            chunk,
          );
          lastMessageId = sent.message_id;
        }
        return lastMessageId !== undefined
          ? { channelMessageId: String(lastMessageId) }
          : {};
      }
      case "react": {
        await bot.api.setMessageReaction(chatId, Number(payload.messageId), [
          { type: "emoji", emoji: payload.emoji as never },
        ]);
        return {};
      }
      case "edit": {
        await sendWithFormatting(
          (text, parseMode) =>
            bot.api.editMessageText(chatId, Number(payload.messageId), text, { ...parseMode }),
          payload.text,
        );
        return { channelMessageId: payload.messageId };
      }
    }
  } catch (error) {
    throw asDeliveryError(error);
  }
}

/// Sends markdown as Telegram HTML; if Telegram rejects the entities (the
/// converter produced something its parser dislikes), falls back to the raw
/// text so formatting never blocks delivery.
async function sendWithFormatting<T>(
  send: (text: string, parseMode: { parse_mode?: "HTML" }) => Promise<T>,
  markdown: string,
): Promise<T> {
  try {
    return await send(renderTelegramHtml(markdown), { parse_mode: "HTML" });
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    if (!/parse entities/i.test(message)) {
      throw error;
    }
    console.warn(`telegram: HTML formatting rejected, sending plain text: ${message}`);
    return send(markdown, {});
  }
}

function asDeliveryError(error: unknown): DeliveryError {
  const message = error instanceof Error ? error.message : String(error);
  // Telegram 4xx responses (bad message id, no permission, unsupported
  // reaction) will not succeed on retry; transport errors might.
  const retryable = !/400|bad request|message to react|message can't|not found|forbidden/i.test(
    message,
  );
  return new DeliveryError(`telegram delivery failed: ${message}`, retryable);
}

async function downloadTelegramPhoto(
  ctx: Context,
  botToken: string,
  message: Message,
): Promise<InboundMedia[]> {
  const sizes = message.photo ?? [];
  // Telegram lists sizes ascending; take the largest one under the cap.
  const candidate = [...sizes]
    .reverse()
    .find((size) => (size.file_size ?? 0) <= MAX_TELEGRAM_IMAGE_BYTES);
  if (!candidate) {
    return [];
  }
  const file = await ctx.api.getFile(candidate.file_id);
  if (!file.file_path) {
    return [];
  }
  const response = await fetch(`https://api.telegram.org/file/bot${botToken}/${file.file_path}`);
  if (!response.ok) {
    throw new Error(`telegram file download failed: ${response.status}`);
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.byteLength > MAX_TELEGRAM_IMAGE_BYTES) {
    return [];
  }
  // Telegram photos are always JPEG re-encodes.
  return [
    {
      base64: bytes.toString("base64"),
      mime: "image/jpeg",
      name: file.file_path.split("/").at(-1) ?? "photo.jpg",
    },
  ];
}

async function downloadTelegramDocument(
  ctx: Context,
  botToken: string,
  message: Message,
  mime: string,
): Promise<InboundMedia[]> {
  const document = message.document;
  if (!document) {
    return [];
  }
  const limit = documentByteLimit(mime);
  if ((document.file_size ?? 0) > limit) {
    return [];
  }
  const file = await ctx.api.getFile(document.file_id);
  if (!file.file_path) {
    return [];
  }
  const response = await fetch(`https://api.telegram.org/file/bot${botToken}/${file.file_path}`);
  if (!response.ok) {
    throw new Error(`telegram file download failed: ${response.status}`);
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.byteLength > limit) {
    return [];
  }
  return [
    {
      base64: bytes.toString("base64"),
      mime,
      name: document.file_name ?? file.file_path.split("/").at(-1) ?? "document",
    },
  ];
}

function senderDisplayName(message: Message): string {
  const from = message.from;
  if (!from) {
    return "unknown";
  }
  const name = [from.first_name, from.last_name].filter(Boolean).join(" ").trim();
  return name || from.username || String(from.id);
}

function messageMentionsBot(
  message: Message,
  botId: number,
  botUsername: string | null,
): boolean {
  const text = message.text ?? message.caption ?? "";
  for (const entity of message.entities ?? message.caption_entities ?? []) {
    if (entity.type === "mention" && botUsername) {
      const mention = text.slice(entity.offset, entity.offset + entity.length);
      if (mention.toLowerCase() === `@${botUsername.toLowerCase()}`) {
        return true;
      }
    }
    if (entity.type === "text_mention" && entity.user.id === botId) {
      return true;
    }
  }
  return false;
}

async function sendTelegramReply(
  ctx: Context,
  text: string,
  replyToMessageId: number,
  replyToMode: ReplyToMode,
  isDirect: boolean,
): Promise<void> {
  const chunks = splitMessageText(text, 4_000);
  for (const [index, chunk] of chunks.entries()) {
    const options = shouldQuoteChunk(replyToMode, isDirect, index)
      ? { reply_parameters: { message_id: replyToMessageId } }
      : {};
    await sendWithFormatting(
      (chunkText, parseMode) => ctx.reply(chunkText, { ...options, ...parseMode }),
      chunk,
    );
  }
}
