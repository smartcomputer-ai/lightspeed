import type { ChannelProvider, ChannelRoute } from "./channel.js";

export type ChannelDeliveryOperation =
  | {
      type: "send";
      text: string;
      replyTo?: string | null;
      replyContext?: { senderId: string; text: string };
    }
  | { type: "edit"; messageId: string; text: string }
  | { type: "react"; messageId: string; emoji: string };

export interface ChannelDeliveryCommandV1 {
  version: 1;
  invocationId: string;
  idempotencyKey: string;
  route: ChannelRoute;
  operation: ChannelDeliveryOperation;
}

export interface ChannelDeliveryResultV1 {
  version: 1;
  provider: ChannelProvider;
  messageIds: string[];
}

export interface ChannelDeliveryActivities {
  deliverChannelMessage(command: ChannelDeliveryCommandV1): Promise<ChannelDeliveryResultV1>;
}

export class ChannelDeliveryError extends Error {
  constructor(
    message: string,
    readonly retryable: boolean,
  ) {
    super(message);
    this.name = "ChannelDeliveryError";
  }
}

export function parseToolOperation(toolId: string, value: unknown): ChannelDeliveryOperation {
  const args = record(value, "tool arguments");
  switch (toolId) {
    case "channels.message_send.v1": {
      const text = nonEmptyString(args.text, "text");
      const replyTo = optionalNullableString(args.replyTo, "replyTo");
      return { type: "send", text, ...(replyTo === undefined ? {} : { replyTo }) };
    }
    case "channels.message_edit.v1":
      return {
        type: "edit",
        messageId: nonEmptyString(args.messageId, "messageId"),
        text: nonEmptyString(args.text, "text"),
      };
    case "channels.message_react.v1":
      return {
        type: "react",
        messageId: nonEmptyString(args.messageId, "messageId"),
        emoji: nonEmptyString(args.emoji, "emoji"),
      };
    default:
      throw new TypeError(`unsupported pushed channel tool: ${toolId}`);
  }
}

export function validateDeliveryResult(
  value: ChannelDeliveryResultV1,
  expectedProvider: ChannelProvider,
): ChannelDeliveryResultV1 {
  if (value.version !== 1 || value.provider !== expectedProvider) {
    throw new TypeError("delivery result does not match the command provider");
  }
  if (
    !Array.isArray(value.messageIds) ||
    value.messageIds.length === 0 ||
    value.messageIds.length > 32 ||
    value.messageIds.some((id) => typeof id !== "string" || id.length === 0)
  ) {
    throw new TypeError("delivery result must contain 1 to 32 message ids");
  }
  return value;
}

export function isDeliveryIdempotencyKey(command: ChannelDeliveryCommandV1): boolean {
  return (
    command.idempotencyKey === command.invocationId ||
    command.idempotencyKey.startsWith(`${command.invocationId}:chunk:`)
  );
}

function record(value: unknown, name: string): Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new TypeError(`${name} must be an object`);
  }
  return value as Record<string, unknown>;
}

function nonEmptyString(value: unknown, name: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new TypeError(`${name} must be a non-empty string`);
  }
  return value;
}

function optionalNullableString(value: unknown, name: string): string | null | undefined {
  if (value === undefined || value === null) {
    return value;
  }
  return nonEmptyString(value, name);
}
