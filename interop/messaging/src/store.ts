import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import path from "node:path";
import type { EventCursor } from "@forge/agent-client";
import type { ActivationPolicy } from "./policy.js";

export interface ConversationState {
  sessionId: string;
  cursor?: EventCursor | null;
  updatedAtMs: number;
}

export interface BindingState {
  channel: string;
  accountId: string;
  chatId: string;
  threadId?: string;
  sessionId: string;
  /// Bumped by `/new`; folded into the derived session id.
  generation: number;
  activation: ActivationPolicy;
  cursor?: EventCursor | null;
  updatedAtMs: number;
}

export interface BindingInit {
  channel: string;
  accountId: string;
  chatId: string;
  threadId?: string;
  sessionId: string;
  activation: ActivationPolicy;
}

export interface MessageState {
  status: "processing" | "done";
  updatedAtMs: number;
  replyText?: string;
  error?: string;
}

export interface BridgeState {
  /// Legacy chat-to-session map; migrated into `bindings` on first touch.
  conversations: Record<string, ConversationState>;
  bindings: Record<string, BindingState>;
  messages: Record<string, MessageState>;
}

const EMPTY_STATE: BridgeState = {
  conversations: {},
  bindings: {},
  messages: {},
};

export class JsonBridgeStore {
  private state: BridgeState | null = null;
  private writeChain: Promise<void> = Promise.resolve();

  constructor(private readonly filePath: string) {}

  async getOrCreateBinding(key: string, init: BindingInit): Promise<BindingState> {
    const state = await this.load();
    const existing = state.bindings[key];
    if (existing) {
      return existing;
    }
    const legacy = state.conversations[key];
    const next: BindingState = {
      channel: init.channel,
      accountId: init.accountId,
      chatId: init.chatId,
      ...(init.threadId !== undefined ? { threadId: init.threadId } : {}),
      sessionId: legacy?.sessionId ?? init.sessionId,
      generation: 0,
      activation: init.activation,
      ...(legacy?.cursor !== undefined ? { cursor: legacy.cursor } : {}),
      updatedAtMs: Date.now(),
    };
    state.bindings[key] = next;
    delete state.conversations[key];
    await this.persist();
    return next;
  }

  async getBinding(key: string): Promise<BindingState | null> {
    const state = await this.load();
    return state.bindings[key] ?? null;
  }

  async updateBinding(
    key: string,
    patch: Partial<Pick<BindingState, "activation" | "sessionId" | "generation" | "cursor">>,
  ): Promise<BindingState> {
    const state = await this.load();
    const existing = state.bindings[key];
    if (!existing) {
      throw new Error(`unknown binding: ${key}`);
    }
    const next: BindingState = {
      ...existing,
      ...patch,
      updatedAtMs: Date.now(),
    };
    state.bindings[key] = next;
    await this.persist();
    return next;
  }

  async updateCursor(key: string, cursor: EventCursor | null): Promise<void> {
    const state = await this.load();
    const existing = state.bindings[key];
    if (!existing) {
      return;
    }
    state.bindings[key] = {
      ...existing,
      cursor,
      updatedAtMs: Date.now(),
    };
    await this.persist();
  }

  async beginMessage(key: string): Promise<"new" | "duplicate" | "inflight"> {
    const state = await this.load();
    const existing = state.messages[key];
    if (existing?.status === "done") {
      return "duplicate";
    }
    if (existing?.status === "processing" && Date.now() - existing.updatedAtMs < 15 * 60_000) {
      return "inflight";
    }
    state.messages[key] = {
      status: "processing",
      updatedAtMs: Date.now(),
    };
    pruneMessages(state);
    await this.persist();
    return "new";
  }

  async markMessageDone(key: string, replyText?: string, error?: string): Promise<void> {
    const state = await this.load();
    state.messages[key] = {
      status: "done",
      updatedAtMs: Date.now(),
      ...(replyText !== undefined ? { replyText } : {}),
      ...(error !== undefined ? { error } : {}),
    };
    pruneMessages(state);
    await this.persist();
  }

  async markMessageRetryable(key: string, error: unknown): Promise<void> {
    const state = await this.load();
    state.messages[key] = {
      status: "processing",
      updatedAtMs: 0,
      error: error instanceof Error ? error.message : String(error),
    };
    await this.persist();
  }

  private async load(): Promise<BridgeState> {
    if (this.state) {
      return this.state;
    }
    try {
      this.state = JSON.parse(await readFile(this.filePath, "utf8")) as BridgeState;
    } catch {
      this.state = structuredClone(EMPTY_STATE);
    }
    this.state.conversations ??= {};
    this.state.bindings ??= {};
    this.state.messages ??= {};
    return this.state;
  }

  private async persist(): Promise<void> {
    const state = await this.load();
    const dir = path.dirname(this.filePath);
    const tmp = path.join(dir, `.${path.basename(this.filePath)}.${process.pid}.tmp`);
    this.writeChain = this.writeChain.then(async () => {
      await mkdir(dir, { recursive: true });
      await writeFile(tmp, `${JSON.stringify(state, null, 2)}\n`);
      await rename(tmp, this.filePath);
    });
    await this.writeChain;
  }
}

function pruneMessages(state: BridgeState): void {
  const maxAgeMs = 7 * 24 * 60 * 60_000;
  const cutoff = Date.now() - maxAgeMs;
  for (const [key, message] of Object.entries(state.messages)) {
    if (message.updatedAtMs < cutoff) {
      delete state.messages[key];
    }
  }
}
