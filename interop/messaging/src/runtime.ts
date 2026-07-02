import { TurnDebouncer } from "./batcher.js";
import type { BindingAccessCandidate, BridgeRuntimeConfig } from "./config.js";
import type { ProfileSource } from "@lightspeed/agent-client";
import type { LightspeedContextAppend, LightspeedSessionBridge } from "./lightspeed.js";
import { stableHash, stableSessionId } from "./ids.js";
import {
  classifyInbound,
  formatEnvelope,
  type ActivationPolicy,
  type ControlCommand,
  type GroupActivation,
} from "./policy.js";
import type { BindingState, JsonBridgeStore } from "./store.js";
import { extractTriggeredText } from "./text.js";

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
  /// Stable key for pairing state. Defaults to conversationKey when omitted.
  pairingKey?: string;
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
  /// Profile resolved for this conversation (null = default profile).
  profile?: ProfileSource | null;
  /// Ordered binding candidates resolved for this inbound message.
  bindingCandidates?: readonly BindingAccessCandidate[];
  /// Selected binding id when no explicit candidate list is provided.
  bindingId?: string | null;
  /// Configured session key for the binding, or null to derive one per
  /// conversation. Conversations sharing a key share a session.
  sessionKey?: string | null;
  /// Profile label recorded on the binding (informational; null = default).
  profileLabel?: string | null;
  /// Lazily fetches attached media; only invoked when the message becomes a
  /// permitted context or a user turn. Unauthorized/self/control messages do
  /// not download media.
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
  /// Clears the channel's typing indicator after the turn completes. Channels
  /// without an explicit clear action can omit this and rely on expiry.
  clearTyping?: () => Promise<void>;
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
  contextKeys?: readonly string[];
  sendReply: (text: string) => Promise<void>;
  setTyping?: (() => Promise<void>) | undefined;
  clearTyping?: (() => Promise<void>) | undefined;
}

type PairingResolution =
  | { kind: "continue"; binding: BindingAccessCandidate; paired: boolean }
  | { kind: "handled" };

/// One appended room message in a conversation's unconsumed backlog.
interface RoomBacklogMessage {
  /// Hierarchical per-message base key (`channel.room.<room>.msg.<id>`).
  baseKey: string;
  /// Exact entry keys committed by the append response for this message.
  entryKeys: readonly string[];
}

/// Server admission cap for keys per context/remove call.
const CONTEXT_REMOVE_BATCH_LIMIT = 64;

const TYPING_INTERVAL_MS = 4_500;
const TYPING_MAX_MS = 3 * 60_000;

/// Fires the typing indicator now and keeps refreshing it (channels expire
/// typing state after a few seconds) until the returned stop function runs
/// or the cap is hit.
function startTypingLoop(
  setTyping: () => Promise<void>,
  log: (message: string) => void,
  clearTyping?: () => Promise<void>,
): () => Promise<void> {
  let lastTyping = Promise.resolve();
  let stopped = false;
  const clear = async () => {
    if (!clearTyping) {
      return;
    }
    try {
      await clearTyping();
    } catch (error) {
      log(
        `bridge: clear typing action failed: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
  };
  const fire = () => {
    if (stopped) {
      return;
    }
    lastTyping = setTyping().catch((error) => {
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
      stopped = true;
      void lastTyping.then(clear, clear);
      return;
    }
    fire();
  }, TYPING_INTERVAL_MS);
  timer.unref?.();
  return async () => {
    stopped = true;
    clearInterval(timer);
    await lastTyping;
    await clear();
  };
}

export class MessagingBridgeRuntime {
  private readonly lightspeed: LightspeedSessionBridge;
  private readonly store: JsonBridgeStore;
  private readonly config: BridgeRuntimeConfig;
  private readonly sessionPrefix: string;
  private readonly log: (message: string) => void;
  private readonly queues = new Map<string, Promise<void>>();
  private readonly seenRoomKeys = new Set<string>();
  private readonly turns: TurnDebouncer<PendingTurn>;
  /// Unconsumed room backlog per conversation: room messages appended since
  /// the bridge last started a run there, oldest first. Only these are ever
  /// pruned by retention; anything a run has seen is consumed history and
  /// belongs to server-side compaction. In-memory only by design: a restart
  /// starts empty, which merely delays pruning until new chatter rebuilds the
  /// count — it can never cause a wrong prune.
  private readonly roomBacklogs = new Map<string, RoomBacklogMessage[]>();

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
  }

  async handleInbound(message: NormalizedInbound, policy: ChannelPolicy, options: HandleInboundOptions): Promise<void> {
    const resolution = await this.resolvePairing(message, policy, options);
    if (resolution.kind === "handled") {
      return;
    }
    const selected = resolution.binding;
    const effectiveMessage: NormalizedInbound = {
      ...message,
      bindingId: selected.bindingId,
      profile: selected.profile,
      profileLabel: selected.profileLabel,
      sessionKey: selected.sessionKey,
      turnAllowed: resolution.paired || message.turnAllowed,
    };
    const binding = await this.ensureBinding(effectiveMessage, policy);
    const classification = classifyInbound(
      {
        text: effectiveMessage.text,
        hasMedia: effectiveMessage.fetchMedia !== undefined,
        isDirect: effectiveMessage.isDirect,
        isFromSelf: effectiveMessage.isFromSelf,
        mentionedBot: effectiveMessage.mentionedBot,
        isReplyToBot: effectiveMessage.isReplyToBot,
        turnAllowed: effectiveMessage.turnAllowed,
        controlAllowed: effectiveMessage.controlAllowed,
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
        await this.handleControl(effectiveMessage, classification.command, options);
        return;
      case "roomEvent": {
        // Every allowed room message is ingested eagerly via context/append;
        // the append is awaited before any turn can be queued, so room
        // context always lands ahead of the runs that should see it.
        if (this.seenRoomKeys.has(effectiveMessage.messageKey)) {
          return;
        }
        const appended = await this.ingestMessageContext(
          binding,
          effectiveMessage,
          classification.text,
        );
        if (!appended) {
          // The message stays unmarked so channel redelivery retries the
          // append.
          return;
        }
        this.rememberRoomKey(effectiveMessage.messageKey);
        this.trackRoomBacklog(effectiveMessage, appended.keys);
        if (
          binding.activation === "mention" &&
          appended.keys.length > 0 &&
          appendActivationMatches(appended.activationText, policy)
        ) {
          const state = await this.store.beginMessage(effectiveMessage.messageKey);
          if (state !== "new") {
            return;
          }
          this.turns.push(effectiveMessage.conversationKey, {
            message: effectiveMessage,
            text: classification.text,
            contextKeys: appended.keys,
            sendReply: options.sendReply,
            setTyping: options.setTyping,
            clearTyping: options.clearTyping,
          });
          return;
        }
        // Retention runs only after appends that do not queue a turn: a
        // queued turn is about to consume the whole backlog anyway.
        await this.maybePruneRoomBacklog(effectiveMessage.conversationKey, binding);
        return;
      }
      case "userTurn": {
        const state = await this.store.beginMessage(effectiveMessage.messageKey);
        if (state !== "new") {
          this.log(`bridge: skipped ${state} message ${effectiveMessage.messageKey}`);
          return;
        }
        if (!effectiveMessage.isDirect && binding.activation === "mention") {
          const appended = await this.ingestMessageContext(
            binding,
            effectiveMessage,
            classification.text,
            (error) => this.store.markMessageRetryable(effectiveMessage.messageKey, error),
          );
          if (!appended) {
            return;
          }
          if (appended.keys.length === 0) {
            const errorText =
              appendFailureMessage(appended.failures) ??
              "context append produced no committed entries";
            if (appendFailuresAreRetryable(appended.failures)) {
              await this.store.markMessageRetryable(
                effectiveMessage.messageKey,
                new Error(errorText),
              );
              return;
            }
            try {
              await options.sendReply(`Lightspeed could not ingest this message: ${errorText}`);
              await this.store.markMessageDone(
                effectiveMessage.messageKey,
                undefined,
                errorText,
              );
            } catch (sendError) {
              await this.store.markMessageRetryable(effectiveMessage.messageKey, sendError);
            }
            return;
          }
          this.turns.push(effectiveMessage.conversationKey, {
            message: effectiveMessage,
            text: classification.text,
            contextKeys: appended.keys,
            sendReply: options.sendReply,
            setTyping: options.setTyping,
            clearTyping: options.clearTyping,
          });
          return;
        }
        this.turns.push(effectiveMessage.conversationKey, {
          message: effectiveMessage,
          text: classification.text,
          sendReply: options.sendReply,
          setTyping: options.setTyping,
          clearTyping: options.clearTyping,
        });
        return;
      }
    }
  }

  /// Flush pending debounced work; used by tests and shutdown.
  async flush(): Promise<void> {
    this.turns.flushAll();
    await Promise.allSettled([...this.queues.values()]);
  }

  private async resolvePairing(
    message: NormalizedInbound,
    policy: ChannelPolicy,
    options: HandleInboundOptions,
  ): Promise<PairingResolution> {
    const candidates = bindingCandidatesForMessage(message);
    const pairingCandidates = candidates.filter(
      (candidate): candidate is BindingAccessCandidate & {
        bindingId: string;
        pairing: NonNullable<BindingAccessCandidate["pairing"]>;
      } =>
        candidate.bindingId !== null && candidate.pairing !== null,
    );
    const pairingKey = message.pairingKey ?? message.conversationKey;

    if (pairingCandidates.length === 0) {
      return { kind: "continue", binding: candidates[0] ?? defaultBindingCandidate(message), paired: false };
    }

    const paired = await this.store.getPairing(pairingKey);
    if (paired) {
      const binding = candidates.find((candidate) => candidate.bindingId === paired.bindingId);
      if (binding) {
        return { kind: "continue", binding, paired: true };
      }
    }

    if (message.isFromSelf) {
      return { kind: "handled" };
    }

    const codeText = message.text.trim();
    const matched = codeText
      ? pairingCandidates.find((candidate) => candidate.pairing.code === codeText)
      : undefined;
    if (matched) {
      const state = await this.store.beginMessage(message.messageKey);
      if (state !== "new") {
        return { kind: "handled" };
      }
      await this.store.pairConversation(pairingKey, {
        channel: message.provider,
        accountId: message.accountId,
        chatId: message.chatId,
        bindingId: matched.bindingId,
      });
      const reply = "Paired. You can now message Lightspeed from this chat.";
      await options.sendReply(reply);
      await this.store.markMessageDone(message.messageKey, reply);
      this.log(`bridge: paired ${message.provider} chat ${message.chatId} to binding ${matched.bindingId}`);
      return { kind: "handled" };
    }

    if (!shouldNotifyPairingRequired(message, policy)) {
      return { kind: "handled" };
    }

    const state = await this.store.beginMessage(message.messageKey);
    if (state !== "new") {
      return { kind: "handled" };
    }
    const reply = "This chat is not paired yet. Send the pairing code to connect it.";
    await options.sendReply(reply);
    await this.store.markMessageDone(message.messageKey, reply);
    return { kind: "handled" };
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
      profileLabel: message.profileLabel ?? null,
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
        return [
          `session: ${binding.sessionId}`,
          `profile: ${binding.profileLabel ?? "default"}`,
          `activation: ${binding.activation}`,
          "commands: /activation mention|always|silent, /status",
        ].join("\n");
      }
    }
  }

  private async processTurnBatch(key: string, batch: PendingTurn[]): Promise<void> {
    const first = batch[0];
    if (!first) {
      return;
    }
    const stopTyping = first.setTyping
      ? startTypingLoop(first.setTyping, this.log, first.clearTyping)
      : null;
    try {
      // Room context is already committed at receipt (handleInbound awaits
      // the context/append before queueing any turn), so the run sees it
      // without an explicit drain here.
      // An /activation flip mid-debounce can mix context-ingested turns with
      // plain input turns in one batch. Submit each consecutive same-kind
      // group separately so content already appended to session context is
      // never re-sent as run input.
      for (const group of partitionTurnsByIngest(batch)) {
        await this.submitTurnGroup(key, group);
      }
    } finally {
      await stopTyping?.();
    }
  }

  /// Submits one same-kind group of turns (all context-ingested or all plain
  /// input) and delivers the reply. Replies anchor to the first message of the
  /// group: in a burst, the first message is usually the question and the rest
  /// elaborate on it.
  private async submitTurnGroup(key: string, batch: PendingTurn[]): Promise<void> {
    const first = batch[0];
    if (!first) {
      return;
    }
    try {
      const binding = await this.store.getBinding(key);
      if (!binding) {
        throw new Error(`no binding for conversation ${key}`);
      }
      // Starting any run consumes the conversation's room backlog: entries
      // appended before this submission become history, owned by server-side
      // compaction and never by retention pruning. Clearing on the attempt
      // (success or failure) is always safe — a lost list only delays
      // pruning — whereas keeping it after an ambiguous failure (run
      // started, then failed) could prune context that run already consumed.
      this.roomBacklogs.delete(key);
      const contextKeys = batch.flatMap((turn) => [...(turn.contextKeys ?? [])]);
      const reply =
        contextKeys.length > 0
          ? await this.lightspeed.submitContextTurn({
              provider: first.message.provider,
              accountId: first.message.accountId,
              conversationKey: key,
              sessionId: binding.sessionId,
              profile: first.message.profile ?? null,
              submissionParts: batch.map((turn) => turn.message.messageId),
              contextKeys,
            })
          : await this.lightspeed.submitTurn({
              provider: first.message.provider,
              accountId: first.message.accountId,
              conversationKey: key,
              sessionId: binding.sessionId,
              profile: first.message.profile ?? null,
              submissionParts: batch.map((turn) => turn.message.messageId),
              text: renderTurnText(batch),
              media: await this.fetchBatchMedia(batch),
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

  private async fetchBatchMedia(batch: PendingTurn[]): Promise<InboundMedia[]> {
    const media: InboundMedia[] = [];
    for (const turn of batch) {
      media.push(...(await this.fetchInboundMedia(turn.message)));
    }
    return media;
  }

  private async fetchInboundMedia(message: NormalizedInbound): Promise<InboundMedia[]> {
    if (!message.fetchMedia) {
      return [];
    }
    try {
      return await message.fetchMedia();
    } catch (error) {
      this.log(
        `bridge: media download failed for ${message.messageKey}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
      return [];
    }
  }

  /// Records an appended room message in the conversation's unconsumed
  /// backlog for retention bookkeeping.
  private trackRoomBacklog(message: NormalizedInbound, entryKeys: readonly string[]): void {
    if (this.config.roomRetentionHigh <= 0 || entryKeys.length === 0) {
      return;
    }
    const backlog = this.roomBacklogs.get(message.conversationKey) ?? [];
    backlog.push({ baseKey: roomContextKey(message), entryKeys: [...entryKeys] });
    this.roomBacklogs.set(message.conversationKey, backlog);
  }

  /// Watermarked drop-oldest retention over the unconsumed room backlog (P89
  /// phase 1). When the backlog exceeds the HIGH watermark (in messages), the
  /// oldest messages are removed down to LOW via `context/remove`, so the
  /// room span stays append-only between prunes and one cache invalidation is
  /// amortized over HIGH - LOW messages. Prunes only while the conversation
  /// is idle from the bridge's perspective: no debounced pending turns and no
  /// in-flight turn batch.
  private async maybePruneRoomBacklog(
    conversationKey: string,
    binding: BindingState,
  ): Promise<void> {
    const high = this.config.roomRetentionHigh;
    const low = this.config.roomRetentionLow;
    if (high <= 0) {
      return;
    }
    const backlog = this.roomBacklogs.get(conversationKey);
    if (!backlog || backlog.length <= high) {
      return;
    }
    if (this.turns.pendingCount(conversationKey) > 0 || this.queues.has(conversationKey)) {
      return;
    }
    // Safety net on top of the idle check: never remove a key a queued turn
    // still references as its run/start trigger.
    const queuedKeys = new Set(
      this.turns
        .pendingItems(conversationKey)
        .flatMap((turn) => [...(turn.contextKeys ?? [])]),
    );
    const candidates = backlog
      .slice(0, backlog.length - low)
      .filter((message) => !message.entryKeys.some((key) => queuedKeys.has(key)));
    if (candidates.length === 0) {
      return;
    }
    const evictedKeys = candidates.flatMap((message) => [...message.entryKeys]);
    const failedKeys = new Set<string>();
    try {
      for (let start = 0; start < evictedKeys.length; start += CONTEXT_REMOVE_BATCH_LIMIT) {
        const chunk = evictedKeys.slice(start, start + CONTEXT_REMOVE_BATCH_LIMIT);
        const results = await this.lightspeed.removeContext(binding.sessionId, chunk);
        for (const result of results) {
          // `absent` counts as pruned: the key is gone either way.
          if (result.status === "failed") {
            failedKeys.add(result.key);
            this.log(
              `bridge: context remove failed for ${result.key}: ${
                result.failure?.kind ?? "unknown"
              }: ${result.failure?.message ?? ""}`,
            );
          }
        }
      }
    } catch (error) {
      // Keep the backlog untouched and retry on the next trigger. Chunks that
      // were already removed retry as per-key `absent` no-ops.
      this.log(
        `bridge: room backlog prune failed for ${conversationKey}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
      return;
    }
    // Messages with a failed key stay in the backlog for the next attempt;
    // fully removed (or absent) messages drop out. Filter the live list, not
    // the snapshot: new messages may have been appended while awaiting.
    const pruned = new Set(
      candidates.filter((message) => !message.entryKeys.some((key) => failedKeys.has(key))),
    );
    const current = this.roomBacklogs.get(conversationKey) ?? [];
    this.roomBacklogs.set(
      conversationKey,
      current.filter((message) => !pruned.has(message)),
    );
    this.log(
      `bridge: pruned ${pruned.size} room message(s) from the unconsumed backlog in ${conversationKey}`,
    );
  }

  /// Downloads the message's media and appends message text plus media as
  /// session context. Per-entry failures are logged and returned alongside the
  /// committed keys for the caller to handle. When the append call itself
  /// throws, invokes onAppendError, logs, and returns null; callers decide
  /// whether the message stays retryable.
  private async ingestMessageContext(
    binding: BindingState,
    message: NormalizedInbound,
    text: string,
    onAppendError?: (error: unknown) => Promise<void>,
  ): Promise<LightspeedContextAppend | null> {
    const media = await this.fetchInboundMedia(message);
    let appended: LightspeedContextAppend;
    try {
      appended = await this.appendMessageContext(binding.sessionId, message, text, media);
    } catch (error) {
      await onAppendError?.(error);
      this.log(
        `bridge: context append failed for ${message.messageKey}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
      return null;
    }
    this.logAppendFailures(appended.failures, message.messageKey);
    return appended;
  }

  private async appendMessageContext(
    sessionId: string,
    message: NormalizedInbound,
    text: string,
    media: readonly InboundMedia[],
  ) {
    return this.lightspeed.appendMessageContext({
      sessionId,
      profile: message.profile ?? null,
      key: roomContextKey(message),
      text: text
        ? formatEnvelope({
            provider: message.provider,
            chatLabel: message.chatLabel,
            isDirect: message.isDirect,
            senderName: message.senderName,
            timestampMs: message.timestampMs,
            text,
            messageId: message.messageId,
          })
        : "",
      media,
    });
  }

  private logAppendFailures(
    failures: readonly { key: string; kind: string; message: string }[],
    messageKey: string,
  ): void {
    for (const failure of failures) {
      this.log(
        `bridge: context append failed for ${messageKey} ${failure.key}: ${failure.kind}: ${failure.message}`,
      );
    }
  }
}

/// True when appended context should activate a mention-mode turn. Only
/// media-derived activation text (keys under `.media.`, e.g. voice
/// transcripts) is considered: the raw message text was already
/// mention/prefix-checked by the classifier before it landed here as a room
/// event, and the envelope header around submitted text (chat label, sender
/// name) must never trigger on its own.
function appendActivationMatches(
  activationText: readonly { key: string; text: string }[],
  policy: ChannelPolicy,
): boolean {
  return activationText.some(
    (entry) =>
      entry.key.includes(".media.") &&
      mediaActivationCandidates(entry.text).some(
        (text) =>
          extractTriggeredText(text, {
            botUsername: policy.botUsername ?? null,
            mentionNames: policy.mentionNames,
            prefixes: policy.triggerPrefixes,
            requireTrigger: true,
          }) !== null,
      ),
  );
}

function mediaActivationCandidates(text: string): string[] {
  const trimmed = text.trim();
  if (!trimmed) {
    return [];
  }
  const withoutHeader = trimmed.replace(/^\[[^\]\r\n]*\]\s*/, "").trim();
  return withoutHeader && withoutHeader !== trimmed ? [withoutHeader, trimmed] : [trimmed];
}

function appendFailureMessage(
  failures: readonly { key: string; kind: string; message: string }[],
): string | null {
  if (failures.length === 0) {
    return null;
  }
  return failures
    .map((failure) => `${failure.kind}: ${failure.message}`)
    .join("; ");
}

function appendFailuresAreRetryable(
  failures: readonly { key: string; kind: string; message: string }[],
): boolean {
  return failures.some((failure) => failure.kind === "admissionRejected");
}

/// Splits a debounced batch into consecutive same-kind groups: turns whose
/// content was already ingested via context/append (they carry contextKeys)
/// versus plain input turns. Chronological order is preserved; a single-kind
/// batch yields one group.
function partitionTurnsByIngest(batch: PendingTurn[]): PendingTurn[][] {
  const groups: PendingTurn[][] = [];
  let current: PendingTurn[] | undefined;
  let currentIngested: boolean | undefined;
  for (const turn of batch) {
    const ingested = (turn.contextKeys?.length ?? 0) > 0;
    if (!current || ingested !== currentIngested) {
      current = [];
      currentIngested = ingested;
      groups.push(current);
    }
    current.push(turn);
  }
  return groups;
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

/// Hierarchical room context key (P89): a shared per-room prefix makes
/// room-scoped bookkeeping (retention, future summaries) possible, while the
/// per-message suffix stays stable for append idempotency.
function roomContextKey(message: NormalizedInbound): string {
  const room = stableHash([message.provider, message.accountId, message.conversationKey]);
  return `channel.room.${room}.msg.${contextSafeMessageId(message.messageId)}`;
}

/// Context entry keys accept ASCII alphanumerics plus `._-:` (alphanumeric
/// first character, 128 chars total); channel message ids outside a
/// conservative subset are hashed. Dots are deliberately excluded from raw
/// ids so a message id can never fake the `.media.` key segment that
/// activation matching treats as transcript-derived.
function contextSafeMessageId(messageId: string): string {
  return /^[A-Za-z0-9][A-Za-z0-9_-]{0,47}$/.test(messageId)
    ? messageId
    : stableHash([messageId]);
}

function bindingCandidatesForMessage(message: NormalizedInbound): BindingAccessCandidate[] {
  if (message.bindingCandidates && message.bindingCandidates.length > 0) {
    return [...message.bindingCandidates];
  }
  return [defaultBindingCandidate(message)];
}

function defaultBindingCandidate(message: NormalizedInbound): BindingAccessCandidate {
  return {
    bindingId: message.bindingId ?? null,
    pairing: null,
    profile: message.profile ?? null,
    profileLabel: message.profileLabel ?? null,
    sessionKey: message.sessionKey ?? null,
  };
}

function shouldNotifyPairingRequired(message: NormalizedInbound, policy: ChannelPolicy): boolean {
  if (message.isDirect) {
    return true;
  }
  if (message.mentionedBot || message.isReplyToBot) {
    return true;
  }
  return (
    extractTriggeredText(message.text, {
      botUsername: policy.botUsername ?? null,
      mentionNames: policy.mentionNames,
      prefixes: policy.triggerPrefixes,
      requireTrigger: true,
    }) !== null
  );
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
