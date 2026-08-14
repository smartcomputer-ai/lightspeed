import type {
  ChannelDeliveryActivities,
  ChannelDeliveryCommandV1,
  ChannelDeliveryResultV1,
} from "../contracts/delivery.js";
import { isDeliveryIdempotencyKey } from "../contracts/delivery.js";

/** C0 protocol probe. Real provider adapters replace this worker in C1. */
export function createFakeDeliveryActivities(): ChannelDeliveryActivities {
  return {
    async deliverChannelMessage(command) {
      validateCommand(command);
      const targetId =
        command.operation.type === "send"
          ? `fake:${command.idempotencyKey}`
          : command.operation.messageId;
      return {
        version: 1,
        provider: command.route.provider,
        messageIds: [targetId],
      };
    },
  };
}

function validateCommand(command: ChannelDeliveryCommandV1): void {
  if (command.version !== 1 || !isDeliveryIdempotencyKey(command)) {
    throw new TypeError("invalid fake delivery command identity");
  }
}

export type { ChannelDeliveryResultV1 };
