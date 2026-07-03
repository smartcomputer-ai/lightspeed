import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import path from "node:path";
import type { EventCursor } from "@lightspeed/agent-client";
import type { ActivationPolicy } from "./policy.js";

export interface BindingState {
  channel: string;
  accountId: string;
  chatId: string;
  threadId?: string;
  sessionId: string;
  /// Profile label applied when the bound session was created (null = default).
  profileLabel?: string | null;
  /// Binding-rule id whose gateway credentials this conversation uses
  /// (null = the bridge's default connection). Persisted so the runtime and
  /// outbox routing survive restarts; the credential itself resolves from
  /// live config by rule id, so key rotation needs no state migration.
  authBindingId?: string | null;
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
  profileLabel?: string | null;
  authBindingId?: string | null;
  activation: ActivationPolicy;
}

export interface MessageState {
  status: "processing" | "done";
  updatedAtMs: number;
  replyText?: string;
  error?: string;
}

export interface PairingState {
  channel: string;
  accountId: string;
  chatId: string;
  bindingId: string;
  pairedAtMs: number;
  updatedAtMs: number;
}

export interface PairingInit {
  channel: string;
  accountId: string;
  chatId: string;
  bindingId: string;
}

export interface BridgeState {
  bindings: Record<string, BindingState>;
  pairings: Record<string, PairingState>;
  messages: Record<string, MessageState>;
}

const EMPTY_STATE: BridgeState = {
  bindings: {},
  pairings: {},
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
      const next = refreshBinding(existing, init);
      if (next === existing) {
        return existing;
      }
      state.bindings[key] = next;
      await this.persist();
      return next;
    }
    const next: BindingState = {
      channel: init.channel,
      accountId: init.accountId,
      chatId: init.chatId,
      ...(init.threadId !== undefined ? { threadId: init.threadId } : {}),
      sessionId: init.sessionId,
      profileLabel: init.profileLabel ?? null,
      authBindingId: init.authBindingId ?? null,
      activation: init.activation,
      updatedAtMs: Date.now(),
    };
    state.bindings[key] = next;
    await this.persist();
    return next;
  }

  async getBinding(key: string): Promise<BindingState | null> {
    const state = await this.load();
    return state.bindings[key] ?? null;
  }

  async findBindingBySession(sessionId: string): Promise<BindingState | null> {
    const state = await this.load();
    return (
      Object.values(state.bindings).find((binding) => binding.sessionId === sessionId) ?? null
    );
  }

  async updateBinding(
    key: string,
    patch: Partial<Pick<BindingState, "activation" | "sessionId" | "cursor">>,
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

  async getPairing(key: string): Promise<PairingState | null> {
    const state = await this.load();
    return state.pairings[key] ?? null;
  }

  async pairConversation(key: string, init: PairingInit): Promise<PairingState> {
    const state = await this.load();
    const now = Date.now();
    const existing = state.pairings[key];
    const next: PairingState = {
      channel: init.channel,
      accountId: init.accountId,
      chatId: init.chatId,
      bindingId: init.bindingId,
      pairedAtMs: existing?.pairedAtMs ?? now,
      updatedAtMs: now,
    };
    state.pairings[key] = next;
    await this.persist();
    return next;
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
    this.state.bindings ??= {};
    this.state.pairings ??= {};
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

function refreshBinding(existing: BindingState, init: BindingInit): BindingState {
  const profileLabel = init.profileLabel ?? null;
  const authBindingId = init.authBindingId ?? null;
  const threadId = init.threadId;
  const changed =
    existing.channel !== init.channel ||
    existing.accountId !== init.accountId ||
    existing.chatId !== init.chatId ||
    existing.threadId !== threadId ||
    existing.sessionId !== init.sessionId ||
    (existing.profileLabel ?? null) !== profileLabel ||
    (existing.authBindingId ?? null) !== authBindingId;
  if (!changed) {
    return existing;
  }
  const next: BindingState = {
    ...existing,
    channel: init.channel,
    accountId: init.accountId,
    chatId: init.chatId,
    sessionId: init.sessionId,
    profileLabel,
    authBindingId,
    updatedAtMs: Date.now(),
  };
  if (threadId === undefined) {
    delete next.threadId;
  } else {
    next.threadId = threadId;
  }
  return next;
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
