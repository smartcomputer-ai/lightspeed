import type { ForgeSessionBridge } from "./forge.js";
import type { JsonBridgeStore } from "./store.js";

export interface InboundTextMessage {
  accountId: string;
  conversationKey: string;
  conversationParts: readonly unknown[];
  messageId: string;
  messageKey: string;
  provider: string;
  text: string;
}

export interface HandleInboundOptions {
  sendReply: (text: string) => Promise<void>;
}

export interface MessagingBridgeRuntimeOptions {
  forge: ForgeSessionBridge;
  store: JsonBridgeStore;
  log?: (message: string) => void;
}

export class MessagingBridgeRuntime {
  private readonly forge: ForgeSessionBridge;
  private readonly store: JsonBridgeStore;
  private readonly log: (message: string) => void;
  private readonly queues = new Map<string, Promise<void>>();

  constructor(options: MessagingBridgeRuntimeOptions) {
    this.forge = options.forge;
    this.store = options.store;
    this.log = options.log ?? console.log;
  }

  async handleInboundText(message: InboundTextMessage, options: HandleInboundOptions): Promise<void> {
    const state = await this.store.beginMessage(message.messageKey);
    if (state !== "new") {
      this.log(`bridge: skipped ${state} message ${message.messageKey}`);
      return;
    }

    const previous = this.queues.get(message.conversationKey) ?? Promise.resolve();
    const queued = previous
      .catch(() => undefined)
      .then(() => this.processMessage(message, options));
    const cleanup = queued.finally(() => {
      if (this.queues.get(message.conversationKey) === cleanup) {
        this.queues.delete(message.conversationKey);
      }
    });
    this.queues.set(message.conversationKey, cleanup);
    await queued;
  }

  private async processMessage(
    message: InboundTextMessage,
    options: HandleInboundOptions,
  ): Promise<void> {
    try {
      const reply = await this.forge.submitText(message);
      await options.sendReply(reply.text);
      await this.store.markMessageDone(message.messageKey, reply.text);
      this.log(`bridge: answered ${message.provider} message ${message.messageKey}`);
    } catch (error) {
      const errorText = errorMessage(error);
      const userText = `Forge could not answer this message: ${errorText}`;
      try {
        await options.sendReply(userText);
        await this.store.markMessageDone(message.messageKey, undefined, errorText);
      } catch (sendError) {
        await this.store.markMessageRetryable(message.messageKey, sendError);
        throw sendError;
      }
      this.log(`bridge: failed ${message.provider} message ${message.messageKey}: ${errorText}`);
    }
  }
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}
