import { ApplicationFailure } from "@temporalio/common";
import type {
  ChannelDeliveryActivities,
  ChannelDeliveryCommandV1,
  ChannelDeliveryResultV1,
} from "../../contracts/delivery.js";
import { ChannelDeliveryError } from "../../contracts/delivery.js";
import { isDeliveryIdempotencyKey } from "../../contracts/delivery.js";
import { renderWhatsAppText } from "../../presentation/whatsapp.js";
import { splitMessageText } from "../../presentation/text.js";

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
  command: ChannelDeliveryCommandV1,
): Promise<ChannelDeliveryResultV1> {
  if (
    command.version !== 1 ||
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
      for (const [index, chunk] of splitMessageText(command.operation.text, 3_500).entries()) {
        const options =
          index !== 0 || command.operation.replyTo == null || command.operation.replyContext === undefined
            ? undefined
            : {
                quoted: {
                  key: {
                    remoteJid: jid,
                    id: command.operation.replyTo,
                    fromMe: false,
                    ...(jid.endsWith("@g.us")
                      ? { participant: command.operation.replyContext.senderId }
                      : {}),
                  },
                  message: { conversation: command.operation.replyContext.text },
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
      return { version: 1, provider: "whatsapp", messageIds: ids };
    }
    case "edit": {
      const messageId = nonEmptyMessageId(command.operation.messageId);
      await config.api.sendMessage(jid, {
        text: renderWhatsAppText(command.operation.text),
        edit: { remoteJid: jid, id: messageId, fromMe: true },
      });
      return { version: 1, provider: "whatsapp", messageIds: [messageId] };
    }
    case "react": {
      const messageId = nonEmptyMessageId(command.operation.messageId);
      await config.api.sendMessage(jid, {
        react: {
          text: command.operation.emoji,
          key: { remoteJid: jid, id: messageId, fromMe: false },
        },
      });
      return { version: 1, provider: "whatsapp", messageIds: [messageId] };
    }
  }
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
