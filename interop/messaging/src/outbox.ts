import type { ForgeClient, OutboundMessageView } from "@forge/agent-client";
import type { BindingState, JsonBridgeStore } from "./store.js";

export interface DeliveryResult {
  channelMessageId?: string;
}

export class DeliveryError extends Error {
  constructor(
    message: string,
    readonly retryable: boolean,
  ) {
    super(message);
  }
}

/// Delivers outbox payloads to one channel account. Implemented by the
/// Telegram and WhatsApp adapters.
export interface ChannelDeliverer {
  channel: string;
  accountId: string;
  deliver(binding: BindingState, payload: OutboundMessageView["payload"]): Promise<DeliveryResult>;
}

export interface OutboxTailerOptions {
  client: Pick<ForgeClient, "call">;
  store: JsonBridgeStore;
  deliverers: readonly ChannelDeliverer[];
  waitMs?: number;
  limit?: number;
  log?: (message: string) => void;
}

/// Tails `outbox/read`, resolves each entry's session to a chat binding, and
/// delivers through the matching channel adapter. Single consumer: the
/// cursor restarts at 0 on bridge restart, which re-reads unacked entries.
export class OutboxTailer {
  private readonly client: Pick<ForgeClient, "call">;
  private readonly store: JsonBridgeStore;
  private readonly deliverers: readonly ChannelDeliverer[];
  private readonly waitMs: number;
  private readonly limit: number;
  private readonly log: (message: string) => void;
  private afterSeq = 0;
  private stopped = false;
  private loop: Promise<void> | null = null;

  constructor(options: OutboxTailerOptions) {
    this.client = options.client;
    this.store = options.store;
    this.deliverers = options.deliverers;
    this.waitMs = options.waitMs ?? 25_000;
    this.limit = options.limit ?? 64;
    this.log = options.log ?? console.log;
  }

  start(): void {
    if (!this.loop) {
      this.loop = this.run();
    }
  }

  async stop(): Promise<void> {
    this.stopped = true;
    await this.loop?.catch(() => undefined);
  }

  /// One read-deliver-ack cycle; exposed for tests.
  async tick(): Promise<number> {
    const page = await this.client.call("outbox/read", {
      after: this.afterSeq,
      limit: this.limit,
      waitMs: this.waitMs,
    });
    for (const entry of page.result.entries) {
      await this.deliverEntry(entry);
      this.afterSeq = Math.max(this.afterSeq, entry.seq);
    }
    return page.result.entries.length;
  }

  private async run(): Promise<void> {
    while (!this.stopped) {
      try {
        await this.tick();
      } catch (error) {
        this.log(
          `bridge: outbox tail failed: ${error instanceof Error ? error.message : String(error)}`,
        );
        await sleep(3_000);
      }
    }
  }

  private async deliverEntry(entry: OutboundMessageView): Promise<void> {
    const binding = await this.store.findBindingBySession(entry.sessionId);
    if (!binding) {
      await this.ack(entry, {
        type: "failed",
        error: `no channel binding for session ${entry.sessionId}`,
        retryable: false,
      });
      return;
    }
    const deliverer = this.deliverers.find(
      (candidate) =>
        candidate.channel === binding.channel && candidate.accountId === binding.accountId,
    );
    if (!deliverer) {
      // The owning channel may be temporarily disconnected; retry later.
      await this.ack(entry, {
        type: "failed",
        error: `channel ${binding.channel}/${binding.accountId} is not connected`,
        retryable: true,
      });
      return;
    }
    try {
      const result = await deliverer.deliver(binding, entry.payload);
      await this.ack(entry, {
        type: "delivered",
        ...(result.channelMessageId !== undefined
          ? { channelMessageId: result.channelMessageId }
          : {}),
      });
      this.log(`bridge: delivered outbox ${entry.outboxId} to ${binding.channel}`);
      if (entry.payload.type === "send" && result.channelMessageId !== undefined) {
        await this.recordDeliveredSend(entry, binding, result.channelMessageId);
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      const retryable = !(error instanceof DeliveryError) || error.retryable;
      await this.ack(entry, {
        type: "failed",
        error: message,
        retryable,
      });
      this.log(
        `bridge: delivery of ${entry.outboxId} failed (${retryable ? "will retry" : "parked"}): ${message}`,
      );
    }
  }

  /// Records the delivered channel message id back into the session so the
  /// model can target its own messages with message_edit / reply_to.
  private async recordDeliveredSend(
    entry: OutboundMessageView,
    binding: BindingState,
    channelMessageId: string,
  ): Promise<void> {
    if (entry.payload.type !== "send") {
      return;
    }
    const excerpt =
      entry.payload.text.length > 120
        ? `${entry.payload.text.slice(0, 120)}…`
        : entry.payload.text;
    try {
      await this.client.call("context/append", {
        sessionId: entry.sessionId,
        entries: [
          {
            key: `channel.sent.${entry.outboxId}`,
            item: {
              type: "text",
              text: `[${binding.channel}] you sent message #${channelMessageId}: ${excerpt}`,
            },
          },
        ],
      });
    } catch (error) {
      this.log(
        `bridge: failed to record delivered message id for ${entry.outboxId}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  }

  private async ack(
    entry: OutboundMessageView,
    result:
      | { type: "delivered"; channelMessageId?: string }
      | { type: "failed"; error: string; retryable: boolean },
  ): Promise<void> {
    await this.client.call("outbox/ack", {
      outboxId: entry.outboxId,
      result,
    });
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
