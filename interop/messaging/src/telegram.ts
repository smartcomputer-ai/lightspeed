import { Bot, type Context } from "grammy";
import type { TelegramBridgeConfig } from "./config.js";
import { stableHash } from "./ids.js";
import type { MessagingBridgeRuntime } from "./runtime.js";
import { extractTriggeredText, splitMessageText } from "./text.js";

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

  if (allowedChatIds.size === 0) {
    console.warn("telegram: TELEGRAM_ALLOWED_CHAT_IDS is empty; all chats can trigger the bridge");
  }

  bot.catch((error) => {
    console.error("telegram: handler failed", error.error);
  });

  bot.on("message:text", async (ctx) => {
    const message = ctx.message;
    const chatId = String(message.chat.id);
    if (allowedChatIds.size > 0 && !allowedChatIds.has(chatId)) {
      return;
    }

    const requireTrigger = config.requireTrigger || message.chat.type !== "private";
    const text = extractTriggeredText(message.text, {
      botUsername,
      mentionNames: config.mentionNames,
      prefixes: config.triggerPrefixes,
      requireTrigger,
    });
    if (text === null) {
      return;
    }
    if (!text) {
      await ctx.reply("Send text after the trigger.");
      return;
    }

    const threadId = message.message_thread_id;
    const conversationParts = ["telegram", config.accountId, chatId, threadId ?? "main"];
    const conversationKey = `telegram:${stableHash(conversationParts)}`;
    const messageKey = `telegram:${stableHash([
      config.accountId,
      chatId,
      threadId ?? "main",
      message.message_id,
    ])}`;

    await runtime.handleInboundText(
      {
        accountId: config.accountId,
        conversationKey,
        conversationParts,
        messageId: String(message.message_id),
        messageKey,
        provider: "telegram",
        text,
      },
      {
        sendReply: async (replyText) => {
          await sendTelegramReply(ctx, replyText, message.message_id);
        },
      },
    );
  });

  const polling = bot.start({ allowed_updates: ["message"] }).catch((error) => {
    console.error("telegram: polling stopped", error);
  });
  console.log(
    `telegram: started @${botUsername ?? me.id} as account ${config.accountId}`,
  );

  return {
    stop: async () => {
      await bot.stop();
      await polling;
    },
  };
}

async function sendTelegramReply(
  ctx: Context,
  text: string,
  replyToMessageId: number,
): Promise<void> {
  for (const chunk of splitMessageText(text, 4_000)) {
    await ctx.reply(chunk, {
      reply_parameters: {
        message_id: replyToMessageId,
      },
    });
  }
}
