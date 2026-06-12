import { Bot, type Context } from "grammy";
import type { Message } from "grammy/types";
import type { TelegramBridgeConfig } from "./config.js";
import { stableHash } from "./ids.js";
import { shouldQuoteChunk, type ReplyToMode } from "./policy.js";
import type {
  ChannelPolicy,
  InboundMedia,
  MessagingBridgeRuntime,
  NormalizedInbound,
} from "./runtime.js";
import { splitMessageText } from "./text.js";

/// Telegram bot file downloads are capped at 20MB; the gateway caps images
/// at 10MB anyway.
const MAX_TELEGRAM_IMAGE_BYTES = 10 * 1024 * 1024;

export interface RunningBridge {
  stop: () => Promise<void>;
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
    if (!text && !hasPhoto) {
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
      text: text || "(sent an image)",
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
        : {}),
    };

    await runtime.handleInbound(inbound, policy, {
      sendReply: async (replyText) => {
        await sendTelegramReply(ctx, replyText, message.message_id, config.replyToMode, isDirect);
      },
    });
  };

  bot.on("message:text", handleMessage);
  bot.on("message:photo", handleMessage);

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
  };
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
    if (shouldQuoteChunk(replyToMode, isDirect, index)) {
      await ctx.reply(chunk, {
        reply_parameters: {
          message_id: replyToMessageId,
        },
      });
    } else {
      await ctx.reply(chunk);
    }
  }
}
