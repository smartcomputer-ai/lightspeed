import { ApplicationFailure } from "@temporalio/common";
import type {
  ChannelDeliveryCommand,
  ChannelDeliveryResult,
} from "@lightspeed-ai/sdk/workflow";
import { renderWhatsAppText } from "../../presentation/whatsapp.js";
import { splitMessageText } from "../../presentation/text.js";
import {
  CHANNEL_DELIVERY_VERSION,
  ChannelDeliveryError,
  isDeliveryIdempotencyKey,
  type ChannelDeliveryActivities,
} from "../delivery.js";

export type WhatsAppContent =
  | { text: string; edit?: WhatsAppMessageKey }
  | { react: { text: string; key: WhatsAppMessageKey } };

export interface WhatsAppMessageKey {
  remoteJid: string;
  id: string;
  fromMe: boolean;
  participant?: string;
}

export interface WhatsAppSendOptions {
  quoted?: {
    key: WhatsAppMessageKey;
    message: { conversation: string };
  };
}

export interface WhatsAppDeliveryApi {
  sendMessage(
    jid: string,
    content: WhatsAppContent,
    options?: WhatsAppSendOptions,
  ): Promise<{ key?: { id?: string | null } } | null | undefined>;
}

export interface WhatsAppDeliveryConfig {
  accountId: string;
  api: WhatsAppDeliveryApi;
}

export function createWhatsAppDeliveryActivities(
  config: WhatsAppDeliveryConfig,
): ChannelDeliveryActivities {
  return {
    async deliverChannelMessage(command) {
      try {
        return await deliver(config, command);
      } catch (error) {
        const classified = classifyWhatsAppError(error);
        if (!classified.retryable) {
          throw ApplicationFailure.nonRetryable(classified.message, "WhatsAppDeliveryError");
        }
        throw ApplicationFailure.retryable(classified.message, "WhatsAppDeliveryError");
      }
    },
  };
}

async function deliver(
  config: WhatsAppDeliveryConfig,
  command: ChannelDeliveryCommand,
): Promise<ChannelDeliveryResult> {
  if (
    command.version !== CHANNEL_DELIVERY_VERSION ||
    command.route.provider !== "whatsapp" ||
    command.route.accountId !== config.accountId ||
    !isDeliveryIdempotencyKey(command)
  ) {
    throw new ChannelDeliveryError("WhatsApp delivery command is routed to the wrong worker", false);
  }
  const jid = command.route.chatId;
  switch (command.operation.type) {
    case "send": {
      const ids: string[] = [];
      const replyContext = command.operation.replyContext;
      for (const [index, chunk] of splitMessageText(command.operation.text, 3_500).entries()) {
        const options =
          index !== 0 || command.operation.replyTo == null || replyContext == null
            ? undefined
            : {
                quoted: {
                  key: {
                    remoteJid: jid,
                    id: command.operation.replyTo,
                    fromMe: false,
                    ...(jid.endsWith("@g.us") ? { participant: replyContext.senderId } : {}),
                  },
                  message: { conversation: replyContext.text },
                },
              };
        const content = { text: renderWhatsAppText(chunk) };
        const sent =
          options === undefined
            ? await config.api.sendMessage(jid, content)
            : await config.api.sendMessage(jid, content, options);
        const messageId = sent?.key?.id;
        if (messageId == null || messageId.length === 0) {
          throw new ChannelDeliveryError("WhatsApp accepted no provider message id", true);
        }
        ids.push(messageId);
      }
      return result(ids);
    }
    case "edit": {
      const messageId = nonEmptyMessageId(command.operation.messageId);
      await config.api.sendMessage(jid, {
        text: renderWhatsAppText(command.operation.text),
        edit: { remoteJid: jid, id: messageId, fromMe: true },
      });
      return result([messageId]);
    }
    case "react": {
      const messageId = nonEmptyMessageId(command.operation.messageId);
      // The key must say whose message it is; the conversation workflow
      // knows from the message number's direction.
      await config.api.sendMessage(jid, {
        react: {
          text: command.operation.emoji,
          key: { remoteJid: jid, id: messageId, fromMe: command.operation.fromMe },
        },
      });
      return result([messageId]);
    }
  }
}

function result(messageIds: string[]): ChannelDeliveryResult {
  return { version: CHANNEL_DELIVERY_VERSION, provider: "whatsapp", messageIds };
}

function classifyWhatsAppError(error: unknown): ChannelDeliveryError {
  if (error instanceof ChannelDeliveryError) {
    return error;
  }
  const message = error instanceof Error ? error.message : String(error);
  return new ChannelDeliveryError(`WhatsApp delivery failed: ${message}`, true);
}

function nonEmptyMessageId(value: string): string {
  if (value.length === 0) {
    throw new ChannelDeliveryError("WhatsApp message id must not be empty", false);
  }
  return value;
}
