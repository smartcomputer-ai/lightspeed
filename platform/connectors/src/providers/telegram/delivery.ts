import { ApplicationFailure } from "@temporalio/common";
import type {
  ChannelDeliveryCommand,
  ChannelDeliveryResult,
} from "@lightspeed-ai/client/workflow";
import { renderTelegramHtml } from "../../presentation/telegram.js";
import { splitMessageText } from "../../presentation/text.js";
import {
  CHANNEL_DELIVERY_VERSION,
  ChannelDeliveryError,
  isDeliveryIdempotencyKey,
  type ChannelDeliveryActivities,
} from "../delivery.js";

export interface TelegramSendOptions {
  parse_mode?: "HTML";
  message_thread_id?: number;
  reply_parameters?: { message_id: number };
}

export interface TelegramDeliveryApi {
  sendMessage(
    chatId: string,
    text: string,
    options: TelegramSendOptions,
  ): Promise<{ message_id: number }>;
  editMessageText(
    chatId: string,
    messageId: number,
    text: string,
    options: { parse_mode?: "HTML" },
  ): Promise<unknown>;
  setMessageReaction(
    chatId: string,
    messageId: number,
    reactions: Array<{ type: "emoji"; emoji: string }>,
  ): Promise<unknown>;
}

export interface TelegramDeliveryConfig {
  accountId: string;
  api: TelegramDeliveryApi;
}

export function createTelegramDeliveryActivities(
  config: TelegramDeliveryConfig,
): ChannelDeliveryActivities {
  return {
    async deliverChannelMessage(command) {
      try {
        return await deliver(config, command);
      } catch (error) {
        const classified = classifyTelegramError(error);
        if (!classified.retryable) {
          throw ApplicationFailure.nonRetryable(classified.message, "TelegramDeliveryError");
        }
        throw ApplicationFailure.retryable(classified.message, "TelegramDeliveryError");
      }
    },
  };
}

async function deliver(
  config: TelegramDeliveryConfig,
  command: ChannelDeliveryCommand,
): Promise<ChannelDeliveryResult> {
  if (
    command.version !== CHANNEL_DELIVERY_VERSION ||
    command.route.provider !== "telegram" ||
    command.route.accountId !== config.accountId ||
    !isDeliveryIdempotencyKey(command)
  ) {
    throw new ChannelDeliveryError("telegram delivery command is routed to the wrong worker", false);
  }
  const threadId = parseOptionalPositiveInteger(command.route.threadId, "thread id");
  switch (command.operation.type) {
    case "send": {
      const ids: string[] = [];
      for (const [index, chunk] of splitMessageText(command.operation.text, 4_000).entries()) {
        const options: TelegramSendOptions = {
          ...(threadId === undefined ? {} : { message_thread_id: threadId }),
          ...(index !== 0 || command.operation.replyTo == null
            ? {}
            : {
                reply_parameters: {
                  message_id: parsePositiveInteger(command.operation.replyTo, "reply message id"),
                },
              }),
        };
        const sent = await sendWithFormatting(
          (text, parseMode) =>
            config.api.sendMessage(command.route.chatId, text, { ...options, ...parseMode }),
          chunk,
        );
        ids.push(String(sent.message_id));
      }
      return result(ids);
    }
    case "edit": {
      const messageId = parsePositiveInteger(command.operation.messageId, "message id");
      await sendWithFormatting(
        (text, parseMode) =>
          config.api.editMessageText(command.route.chatId, messageId, text, parseMode),
        command.operation.text,
      );
      return result([String(messageId)]);
    }
    case "react": {
      const messageId = parsePositiveInteger(command.operation.messageId, "message id");
      await config.api.setMessageReaction(command.route.chatId, messageId, [
        { type: "emoji", emoji: command.operation.emoji },
      ]);
      return result([String(messageId)]);
    }
  }
}

function result(messageIds: string[]): ChannelDeliveryResult {
  return { version: CHANNEL_DELIVERY_VERSION, provider: "telegram", messageIds };
}

async function sendWithFormatting<T>(
  send: (text: string, options: { parse_mode?: "HTML" }) => Promise<T>,
  markdown: string,
): Promise<T> {
  try {
    return await send(renderTelegramHtml(markdown), { parse_mode: "HTML" });
  } catch (error) {
    if (!/parse entities/i.test(errorMessage(error))) {
      throw error;
    }
    return send(markdown, {});
  }
}

function classifyTelegramError(error: unknown): ChannelDeliveryError {
  if (error instanceof ChannelDeliveryError) {
    return error;
  }
  const message = errorMessage(error);
  const retryable = !/400|bad request|not found|forbidden|message to react|message can't/i.test(
    message,
  );
  return new ChannelDeliveryError(`telegram delivery failed: ${message}`, retryable);
}

function parseOptionalPositiveInteger(
  value: string | null | undefined,
  name: string,
): number | undefined {
  return value == null ? undefined : parsePositiveInteger(value, name);
}

function parsePositiveInteger(value: string, name: string): number {
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new ChannelDeliveryError(`invalid telegram ${name}: ${value}`, false);
  }
  return parsed;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
