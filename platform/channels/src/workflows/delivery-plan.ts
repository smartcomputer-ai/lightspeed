import type {
  ChannelDeliveryCommandV1,
  ChannelDeliveryOperation,
} from "../contracts/delivery.js";
import type { ChannelRoute } from "../contracts/channel.js";
import { splitMessageText } from "../presentation/text.js";

/**
 * Split sends before scheduling activities so every completed chunk is a
 * separate durable Temporal history event. Edits and reactions stay atomic.
 */
export function planDeliveryCommands(
  invocationId: string,
  route: ChannelRoute,
  operation: ChannelDeliveryOperation,
): ChannelDeliveryCommandV1[] {
  if (operation.type !== "send") {
    return [command(invocationId, invocationId, route, operation)];
  }
  const chunks = splitMessageText(operation.text, 3_500);
  return chunks.map((text, index) =>
    command(
      invocationId,
      chunks.length === 1
        ? invocationId
        : `${invocationId}:chunk:${index + 1}/${chunks.length}`,
      route,
      {
        type: "send",
        text,
        ...(index === 0 && operation.replyTo !== undefined
          ? { replyTo: operation.replyTo }
          : {}),
        ...(index === 0 && operation.replyContext !== undefined
          ? { replyContext: operation.replyContext }
          : {}),
      },
    ),
  );
}

function command(
  invocationId: string,
  idempotencyKey: string,
  route: ChannelRoute,
  operation: ChannelDeliveryOperation,
): ChannelDeliveryCommandV1 {
  return { version: 1, invocationId, idempotencyKey, route, operation };
}
