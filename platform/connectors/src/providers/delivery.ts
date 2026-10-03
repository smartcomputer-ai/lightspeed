import type {
  ChannelDeliveryCommand,
  ChannelDeliveryResult,
} from "@lightspeed-ai/client/workflow";

/** The delivery command/result shape this connector implements. */
export const CHANNEL_DELIVERY_VERSION = 1;

export interface ChannelDeliveryActivities {
  deliverChannelMessage(command: ChannelDeliveryCommand): Promise<ChannelDeliveryResult>;
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

/** `idempotency_key` is the invocation id, or `{invocation}:chunk:{i}/{n}` for a split send. */
export function isDeliveryIdempotencyKey(command: ChannelDeliveryCommand): boolean {
  return (
    command.idempotencyKey === command.invocationId ||
    command.idempotencyKey.startsWith(`${command.invocationId}:chunk:`)
  );
}
