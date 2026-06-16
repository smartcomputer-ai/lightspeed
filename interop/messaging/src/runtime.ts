import { RoomBuffer, TurnDebouncer } from "./batcher.js";
import type { BridgeRuntimeConfig, SessionRecipe } from "./config.js";
import type { LightspeedSessionBridge } from "./lightspeed.js";
import { stableHash, stableSessionId } from "./ids.js";
import {
  classifyInbound,
  formatEnvelope,
  type ActivationPolicy,
  type ControlCommand,
  type GroupActivation,
} from "./policy.js";
import type { BindingState, JsonBridgeStore } from "./store.js";

export interface InboundMedia {
  base64: string;
  mime: string;
  name?: string;
}

export interface NormalizedInbound {
  provider: "telegram" | "whatsapp";
  accountId: string;
  chatId: string;
  threadId?: string;
  conversationKey: string;
  conversationParts: readonly unknown[];
  messageId: string;
  messageKey: string;
  senderId: string;
  senderName: string;
  timestampMs: number;
  text: string;
  isDirect: boolean;
  chatLabel: string;
  mentionedBot: boolean;
  isReplyToBot: boolean;
  isFromSelf: boolean;
  /// Sender is on the channel turn allowlist (may chat at all).
  turnAllowed: boolean;
  /// Sender is on the control allowlist (may run /activation, /status).
  controlAllowed: boolean;
  /// Recipe resolved for this conversation (null = default recipe).
  recipe?: SessionRecipe | null;
  /// Configured session key for the binding, or null to derive one per
  /// conversation. Conversations sharing a key share a session.
  sessionKey?: string | null;
  /// Recipe name recorded on the binding (informational; null = default).
  recipeName?: string | null;
  /// Lazily fetches attached media; only invoked when the message becomes a
  /// user turn, so ignored chatter never downloads anything.
  fetchMedia?: () => Promise<InboundMedia[]>;
}

export interface ChannelPolicy {
  triggerPrefixes: readonly string[];
  mentionNames: readonly string[];
  botUsername?: string | null;
  groupActivation: GroupActivation;
}

export interface HandleInboundOptions {
  sendReply: (text: string) => Promise<void>;
  /// Fires the channel's typing indicator; re-invoked periodically while a
  /// turn is running.
  setTyping?: () => Promise<void>;
}

export interface MessagingBridgeRuntimeOptions {
  lightspeed: LightspeedSessionBridge;
  store: JsonBridgeStore;
  runtime: BridgeRuntimeConfig;
  sessionPrefix: string;
  log?: (message: string) => void;
}

interface PendingTurn {
  message: NormalizedInbound;
  text: string;
  sendReply: (text: string) => Promise<void>;
  setTyping?: (() => Promise<void>) | undefined;
}

const TYPING_INTERVAL_MS = 4_500;
const TYPING_MAX_MS = 3 * 60_000;

/// Fires the typing indicator now and keeps refreshing it (channels expire
/// typing state after a few seconds) until the returned stop function runs
/// or the cap is hit.
function startTypingLoop(
  setTyping: () => Promise<void>,
  log: (message: string) => void,
): () => void {
  const fire = () => {
    setTyping().catch((error) => {
      log(
        `bridge: typing action failed: ${error instanceof Error ? error.message : String(error)}`,
      );
    });
  };
  fire();
  const startedAt = Date.now();
  const timer = setInterval(() => {
    if (Date.now() - startedAt > TYPING_MAX_MS) {
      clearInterval(timer);
      return;
    }
    fire();
  }, TYPING_INTERVAL_MS);
  timer.unref?.();
  return () => clearInterval(timer);
}

export class MessagingBridgeRuntime {
  private readonly lightspeed: LightspeedSessionBridge;
  private readonly store: JsonBridgeStore;
  private readonly config: BridgeRuntimeConfig;
  private readonly sessionPrefix: string;
  private readonly log: (message: string) => void;
  private readonly queues = new Map<string, Promise<void>>();
  private readonly seenRoomKeys = new Set<string>();
  /// Recipe resolved per conversation, so room-event flushes (which only carry
  /// the binding) can provision the session the same way turns do.
  private readonly recipeByConversation = new Map<string, SessionRecipe | null>();
  private readonly turns: TurnDebouncer<PendingTurn>;
  private readonly rooms: RoomBuffer;

  constructor(options: MessagingBridgeRuntimeOptions) {
    this.lightspeed = options.lightspeed;
    this.store = options.store;
    this.config = options.runtime;
    this.sessionPrefix = options.sessionPrefix;
    this.log = options.log ?? console.log;
    this.turns = new TurnDebouncer<PendingTurn>({
      debounceMs: this.config.debounceMs,
      maxBatch: this.config.turnMaxBatch,
      maxWaitMs: this.config.turnMaxWaitMs,
      onFlush: (key, batch) => {
        this.enqueue(key, () => this.processTurnBatch(key, batch));
      },
    });
    this.rooms = new RoomBuffer({
      flushMs: this.config.roomFlushMs,
      flushMax: this.config.roomFlushMax,
      budget: this.config.roomBudget,
      log: this.log,
      onFlush: async (key, events, dropped) => {
        const binding = await this.store.getBinding(key);
        if (!binding) {
          return;
        }
        const flushed =
          dropped > 0 && events.length > 0 && events[0]
            ? [
                {
                  ...events[0],
                  text: `[${dropped} earlier message(s) in this chat were dropped]\n${events[0].text}`,
                },
                ...events.slice(1),
              ]
            : events;
        await this.lightspeed.appendRoomEvents(
          binding.sessionId,
          flushed,
          this.recipeByConversation.get(key) ?? null,
        );
        this.log(`bridge: appended ${events.length} room event(s) for ${key}`);
      },
    });
  }

  async handleInbound(message: NormalizedInbound, policy: ChannelPolicy, options: HandleInboundOptions): Promise<void> {
    const binding = await this.ensureBinding(message, policy);
    this.recipeByConversation.set(message.conversationKey, message.recipe ?? null);
    const classification = classifyInbound(
      {
        text: message.text,
        isDirect: message.isDirect,
        isFromSelf: message.isFromSelf,
        mentionedBot: message.mentionedBot,
        isReplyToBot: message.isReplyToBot,
        turnAllowed: message.turnAllowed,
        controlAllowed: message.controlAllowed,
      },
      {
        activation: binding.activation,
        triggerPrefixes: policy.triggerPrefixes,
        mentionNames: policy.mentionNames,
        botUsername: policy.botUsername ?? null,
      },
    );

    switch (classification.kind) {
      case "drop":
        return;
      case "denied": {
        if (classification.notify) {
          await options.sendReply(
            "You are not authorized to use this assistant.",
          );
        }
        return;
      }
      case "control":
        await this.handleControl(message, classification.command, options);
        return;
      case "roomEvent": {
        if (this.seenRoomKeys.has(message.messageKey)) {
          return;
        }
        this.rememberRoomKey(message.messageKey);
        this.rooms.push(message.conversationKey, {
          key: roomContextKey(message),
          text: formatEnvelope({
            provider: message.provider,
            chatLabel: message.chatLabel,
            isDirect: message.isDirect,
            senderName: message.senderName,
            timestampMs: message.timestampMs,
            text: classification.text,
            messageId: message.messageId,
          }),
        });
        return;
      }
      case "userTurn": {
        const state = await this.store.beginMessage(message.messageKey);
        if (state !== "new") {
          this.log(`bridge: skipped ${state} message ${message.messageKey}`);
          return;
        }
        this.turns.push(message.conversationKey, {
          message,
          text: classification.text,
          sendReply: options.sendReply,
          setTyping: options.setTyping,
        });
        return;
      }
    }
  }

  /// Flush pending debounced work; used by tests and shutdown.
  async flush(): Promise<void> {
    this.turns.flushAll();
    await this.rooms.drainAll();
    await Promise.allSettled([...this.queues.values()]);
  }

  private async ensureBinding(
    message: NormalizedInbound,
    policy: ChannelPolicy,
  ): Promise<BindingState> {
    const activation: ActivationPolicy = message.isDirect ? "dm" : policy.groupActivation;
    // A configured session key binds multiple conversations to one session;
    // otherwise each conversation derives its own session id.
    const sessionParts =
      message.sessionKey != null && message.sessionKey !== ""
        ? [message.provider, message.accountId, "key", message.sessionKey]
        : message.conversationParts;
    return this.store.getOrCreateBinding(message.conversationKey, {
      channel: message.provider,
      accountId: message.accountId,
      chatId: message.chatId,
      ...(message.threadId !== undefined ? { threadId: message.threadId } : {}),
      sessionId: stableSessionId(this.sessionPrefix, sessionParts),
      recipe: message.recipeName ?? null,
      activation,
    });
  }

  private async handleControl(
    message: NormalizedInbound,
    command: ControlCommand,
    options: HandleInboundOptions,
  ): Promise<void> {
    const state = await this.store.beginMessage(message.messageKey);
    if (state !== "new") {
      return;
    }
    try {
      const reply = await this.executeControl(message, command);
      await options.sendReply(reply);
      await this.store.markMessageDone(message.messageKey, reply);
    } catch (error) {
      const text = error instanceof Error ? error.message : String(error);
      await options.sendReply(`Bridge command failed: ${text}`);
      await this.store.markMessageDone(message.messageKey, undefined, text);
    }
  }

  private async executeControl(
    message: NormalizedInbound,
    command: ControlCommand,
  ): Promise<string> {
    switch (command.kind) {
      case "activation": {
        if (message.isDirect && command.mode !== "silent") {
          return "Direct chats are always active; /activation applies to groups.";
        }
        await this.store.updateBinding(message.conversationKey, {
          activation: message.isDirect ? "dm" : command.mode,
        });
        return `Activation set to ${command.mode}.`;
      }
      case "status": {
        const binding = await this.store.getBinding(message.conversationKey);
        if (!binding) {
          return "No session is bound to this chat yet.";
        }
        const buffered = this.rooms.bufferedCount(message.conversationKey);
        return [
          `session: ${binding.sessionId}`,
          `recipe: ${binding.recipe ?? "default"}`,
          `activation: ${binding.activation}`,
          `buffered room messages: ${buffered}`,
          "commands: /activation mention|always|silent, /status",
        ].join("\n");
      }
    }
  }

  private async processTurnBatch(key: string, batch: PendingTurn[]): Promise<void> {
    // Replies anchor to the first message of the batch: in a burst, the
    // first message is usually the question and the rest elaborate on it.
    const first = batch[0];
    if (!first) {
      return;
    }
    const stopTyping = first.setTyping ? startTypingLoop(first.setTyping, this.log) : null;
    try {
      // Buffered room context lands before the turn so the run sees it.
      await this.rooms.drain(key).catch(() => undefined);

      const binding = await this.store.getBinding(key);
      if (!binding) {
        throw new Error(`no binding for conversation ${key}`);
      }
      const text = renderTurnText(batch);
      const media: InboundMedia[] = [];
      for (const turn of batch) {
        if (!turn.message.fetchMedia) {
          continue;
        }
        try {
          media.push(...(await turn.message.fetchMedia()));
        } catch (error) {
          this.log(
            `bridge: media download failed for ${turn.message.messageKey}: ${
              error instanceof Error ? error.message : String(error)
            }`,
          );
        }
      }
      const reply = await this.lightspeed.submitTurn({
        provider: first.message.provider,
        accountId: first.message.accountId,
        conversationKey: key,
        sessionId: binding.sessionId,
        recipe: first.message.recipe ?? null,
        submissionParts: batch.map((turn) => turn.message.messageId),
        text,
        media,
      });
      if (reply.messagingToolUsed) {
        // The run spoke (or deliberately stayed quiet) via messaging tools;
        // deliveries arrive through the outbox tail. Final text is internal.
        this.log(
          `bridge: ${reply.runId} used messaging tools; final text not delivered in ${key}`,
        );
      } else {
        // Default fallback: no messaging tool was used, so the final
        // assistant text is the reply.
        await first.sendReply(reply.text);
      }
      for (const turn of batch) {
        await this.store.markMessageDone(turn.message.messageKey, reply.text);
      }
      this.log(`bridge: answered ${first.message.provider} batch of ${batch.length} in ${key}`);
    } catch (error) {
      const errorText = error instanceof Error ? error.message : String(error);
      const userText = userFacingTurnFailure(error, errorText);
      try {
        await first.sendReply(userText);
        for (const turn of batch) {
          await this.store.markMessageDone(turn.message.messageKey, undefined, errorText);
        }
      } catch (sendError) {
        for (const turn of batch) {
          await this.store.markMessageRetryable(turn.message.messageKey, sendError);
        }
      }
      this.log(`bridge: failed batch in ${key}: ${errorText}`);
    } finally {
      stopTyping?.();
    }
  }

  private enqueue(key: string, work: () => Promise<void>): void {
    const previous = this.queues.get(key) ?? Promise.resolve();
    const queued = previous.catch(() => undefined).then(work);
    const cleanup = queued.finally(() => {
      if (this.queues.get(key) === cleanup) {
        this.queues.delete(key);
      }
    });
    this.queues.set(key, cleanup);
  }

  private rememberRoomKey(key: string): void {
    this.seenRoomKeys.add(key);
    if (this.seenRoomKeys.size > 4096) {
      const oldest = this.seenRoomKeys.values().next().value;
      if (oldest) {
        this.seenRoomKeys.delete(oldest);
      }
    }
  }
}

function renderTurnText(batch: PendingTurn[]): string {
  // Every turn carries the envelope, including single DMs: the #id markers
  // are what make message_react / message_edit / reply_to targetable.
  return batch
    .map((turn) =>
      formatEnvelope({
        provider: turn.message.provider,
        chatLabel: turn.message.chatLabel,
        isDirect: turn.message.isDirect,
        senderName: turn.message.senderName,
        timestampMs: turn.message.timestampMs,
        text: turn.text,
        messageId: turn.message.messageId,
      }),
    )
    .join("\n");
}

function roomContextKey(message: NormalizedInbound): string {
  return `channel.room.${stableHash([
    message.provider,
    message.accountId,
    message.conversationKey,
    message.messageId,
    message.senderId,
  ])}`;
}

const AUDIO_ADMISSION_FAILURE_KINDS = new Set([
  "unsupported_audio_mime",
  "audio_blob_too_large",
  "audio_duration_too_long",
  "transcoder_unavailable",
  "transcode_failure",
  "transcription_failure",
]);

function userFacingTurnFailure(error: unknown, fallbackMessage: string): string {
  const apiError = apiErrorData(error);
  if (apiError && AUDIO_ADMISSION_FAILURE_KINDS.has(apiError.kind)) {
    return `Lightspeed could not transcribe this audio message: ${
      apiError.message || fallbackMessage
    }`;
  }
  return `Lightspeed could not answer this message: ${fallbackMessage}`;
}

function apiErrorData(error: unknown): { kind: string; message?: string } | null {
  if (typeof error !== "object" || error === null || !("data" in error)) {
    return null;
  }
  const data = (error as { data?: unknown }).data;
  if (typeof data !== "object" || data === null) {
    return null;
  }
  const kind = (data as { kind?: unknown }).kind;
  if (typeof kind !== "string") {
    return null;
  }
  const message = (data as { message?: unknown }).message;
  return {
    kind,
    ...(typeof message === "string" ? { message } : {}),
  };
}
