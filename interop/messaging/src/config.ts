import { readFile } from "node:fs/promises";
import type { GroupActivation, ReplyToMode } from "./policy.js";

export interface LightspeedBridgeConfig {
  endpoint: string;
  cwd?: string | null;
  waitMs: number;
  eventLimit: number;
  sessionPrefix: string;
}

export interface BridgeRuntimeConfig {
  /// Quiet window that batches rapid consecutive messages into one turn.
  debounceMs: number;
  turnMaxBatch: number;
  turnMaxWaitMs: number;
  /// Room-event batching toward context/append.
  roomFlushMs: number;
  roomFlushMax: number;
  /// Max room events buffered per chat between flushes; overflow is dropped.
  roomBudget: number;
}

export interface TelegramBridgeConfig {
  enabled: boolean;
  botToken: string;
  accountId: string;
  allowedChatIds: string[];
  /// Senders allowed to run control commands (/activation, /new, /status).
  allowFrom: string[];
  triggerPrefixes: string[];
  mentionNames: string[];
  groupActivation: GroupActivation;
  /// Reply-quote behavior in groups (`off` | `first` | `all`). Direct chats
  /// never quote.
  replyToMode: ReplyToMode;
}

export interface WhatsAppBridgeConfig {
  enabled: boolean;
  accountId: string;
  authDir: string;
  allowedJids: string[];
  allowFrom: string[];
  triggerPrefixes: string[];
  mentionNames: string[];
  groupActivation: GroupActivation;
  replyToMode: ReplyToMode;
  printQr: boolean;
}

export interface MessagingBridgeConfig {
  lightspeed: LightspeedBridgeConfig;
  runtime: BridgeRuntimeConfig;
  store: {
    path: string;
  };
  telegram?: TelegramBridgeConfig;
  whatsapp?: WhatsAppBridgeConfig;
}

type PartialConfig = Partial<{
  lightspeed: Partial<LightspeedBridgeConfig>;
  runtime: Partial<BridgeRuntimeConfig>;
  store: Partial<{ path: string }>;
  telegram: Partial<TelegramBridgeConfig>;
  whatsapp: Partial<WhatsAppBridgeConfig>;
}>;

export async function loadBridgeConfig(env: NodeJS.ProcessEnv = process.env): Promise<MessagingBridgeConfig> {
  const fileConfig = env.BRIDGE_CONFIG ? await readJsonConfig(env.BRIDGE_CONFIG) : {};
  const lightspeed = {
    endpoint:
      env.LIGHTSPEED_API_URL ??
      fileConfig.lightspeed?.endpoint ??
      "http://127.0.0.1:18080/rpc",
    cwd: env.LIGHTSPEED_CWD ?? fileConfig.lightspeed?.cwd ?? null,
    waitMs: parsePositiveInt(env.LIGHTSPEED_WAIT_MS, fileConfig.lightspeed?.waitMs ?? 30_000),
    eventLimit: parsePositiveInt(env.LIGHTSPEED_EVENT_LIMIT, fileConfig.lightspeed?.eventLimit ?? 128),
    sessionPrefix: env.BRIDGE_SESSION_PREFIX ?? fileConfig.lightspeed?.sessionPrefix ?? "bridge",
  };
  const runtime: BridgeRuntimeConfig = {
    debounceMs: parsePositiveInt(env.BRIDGE_DEBOUNCE_MS, fileConfig.runtime?.debounceMs ?? 500),
    turnMaxBatch: parsePositiveInt(
      env.BRIDGE_TURN_MAX_BATCH,
      fileConfig.runtime?.turnMaxBatch ?? 10,
    ),
    turnMaxWaitMs: parsePositiveInt(
      env.BRIDGE_TURN_MAX_WAIT_MS,
      fileConfig.runtime?.turnMaxWaitMs ?? 2_500,
    ),
    roomFlushMs: parsePositiveInt(env.BRIDGE_ROOM_FLUSH_MS, fileConfig.runtime?.roomFlushMs ?? 30_000),
    roomFlushMax: parsePositiveInt(
      env.BRIDGE_ROOM_FLUSH_MAX,
      fileConfig.runtime?.roomFlushMax ?? 20,
    ),
    roomBudget: parsePositiveInt(env.BRIDGE_ROOM_BUDGET, fileConfig.runtime?.roomBudget ?? 50),
  };
  const telegramToken = env.TELEGRAM_BOT_TOKEN ?? fileConfig.telegram?.botToken ?? "";
  const telegramEnabled =
    parseBoolean(env.TELEGRAM_ENABLED, fileConfig.telegram?.enabled ?? Boolean(telegramToken));
  const whatsappEnabled = parseBoolean(env.WHATSAPP_ENABLED, fileConfig.whatsapp?.enabled ?? false);

  const config: MessagingBridgeConfig = {
    lightspeed,
    runtime,
    store: {
      path: env.BRIDGE_STATE_PATH ?? fileConfig.store?.path ?? ".bridge-state.json",
    },
  };
  if (telegramEnabled) {
    config.telegram = {
      enabled: true,
      botToken: requireValue("TELEGRAM_BOT_TOKEN", telegramToken),
      accountId: env.TELEGRAM_ACCOUNT_ID ?? fileConfig.telegram?.accountId ?? "default",
      allowedChatIds: csv(env.TELEGRAM_ALLOWED_CHAT_IDS, fileConfig.telegram?.allowedChatIds),
      allowFrom: csv(env.TELEGRAM_ALLOW_FROM, fileConfig.telegram?.allowFrom),
      triggerPrefixes: csv(
        env.TELEGRAM_TRIGGER_PREFIXES,
        fileConfig.telegram?.triggerPrefixes ?? ["/ask", "/lightspeed"],
      ),
      mentionNames: csv(env.TELEGRAM_MENTION_NAMES, fileConfig.telegram?.mentionNames),
      groupActivation: parseGroupActivation(
        "TELEGRAM_GROUP_ACTIVATION",
        env.TELEGRAM_GROUP_ACTIVATION,
        fileConfig.telegram?.groupActivation,
      ),
      replyToMode: parseReplyToMode(
        "TELEGRAM_REPLY_TO_MODE",
        env.TELEGRAM_REPLY_TO_MODE,
        fileConfig.telegram?.replyToMode,
      ),
    };
  }
  if (whatsappEnabled) {
    config.whatsapp = {
      enabled: true,
      accountId: env.WHATSAPP_ACCOUNT_ID ?? fileConfig.whatsapp?.accountId ?? "default",
      authDir: env.WHATSAPP_AUTH_DIR ?? fileConfig.whatsapp?.authDir ?? ".whatsapp-auth",
      allowedJids: csv(env.WHATSAPP_ALLOWED_JIDS, fileConfig.whatsapp?.allowedJids),
      allowFrom: csv(env.WHATSAPP_ALLOW_FROM, fileConfig.whatsapp?.allowFrom),
      triggerPrefixes: csv(
        env.WHATSAPP_TRIGGER_PREFIXES,
        fileConfig.whatsapp?.triggerPrefixes ?? ["/ask", "/lightspeed"],
      ),
      mentionNames: csv(env.WHATSAPP_MENTION_NAMES, fileConfig.whatsapp?.mentionNames),
      groupActivation: parseGroupActivation(
        "WHATSAPP_GROUP_ACTIVATION",
        env.WHATSAPP_GROUP_ACTIVATION,
        fileConfig.whatsapp?.groupActivation,
      ),
      replyToMode: parseReplyToMode(
        "WHATSAPP_REPLY_TO_MODE",
        env.WHATSAPP_REPLY_TO_MODE,
        fileConfig.whatsapp?.replyToMode,
      ),
      printQr: parseBoolean(env.WHATSAPP_PRINT_QR, fileConfig.whatsapp?.printQr ?? true),
    };
  }
  return config;
}

async function readJsonConfig(path: string): Promise<PartialConfig> {
  const raw = await readFile(path, "utf8");
  return JSON.parse(raw) as PartialConfig;
}

function csv(value: string | undefined, fallback: readonly string[] | undefined): string[] {
  const source = value === undefined ? fallback ?? [] : value.split(",");
  return source.map((entry) => entry.trim()).filter(Boolean);
}

function parsePositiveInt(value: string | undefined, fallback: number): number {
  if (value === undefined || value.trim() === "") {
    return fallback;
  }
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function parseBoolean(value: string | undefined, fallback: boolean): boolean {
  if (value === undefined || value.trim() === "") {
    return fallback;
  }
  return ["1", "true", "yes", "on"].includes(value.trim().toLowerCase());
}

function parseGroupActivation(
  name: string,
  value: string | undefined,
  fallback: GroupActivation | undefined,
): GroupActivation {
  const candidate = (value ?? fallback ?? "mention").toString().trim().toLowerCase();
  if (candidate === "mention" || candidate === "always" || candidate === "silent") {
    return candidate;
  }
  throw new Error(`${name} must be one of mention, always, silent`);
}

function parseReplyToMode(
  name: string,
  value: string | undefined,
  fallback: ReplyToMode | undefined,
): ReplyToMode {
  const candidate = (value ?? fallback ?? "first").toString().trim().toLowerCase();
  if (candidate === "off" || candidate === "first" || candidate === "all") {
    return candidate;
  }
  throw new Error(`${name} must be one of off, first, all`);
}

function requireValue(name: string, value: string): string {
  if (!value.trim()) {
    throw new Error(`${name} is required`);
  }
  return value;
}
